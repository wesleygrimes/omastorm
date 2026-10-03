#!/usr/bin/env python3
"""Linux recorded-input baseline. Run from any directory; no provider requests."""
import argparse
import datetime
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import socket
import statistics
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parent.parent


def resources(pid, runtime):
    proc = Path(f"/proc/{pid}")
    status = dict(re.findall(r"^(\w+):\s+(\d+)", (proc / "status").read_text(), re.M))
    textures = list((runtime / "omastorm/tex").glob("*"))
    return {
        "rss_kib": int(status["VmRSS"]),
        "peak_rss_kib": int(status["VmHWM"]),
        "threads": int(status["Threads"]),
        "fds": len(list((proc / "fd").iterdir())),
        "textures": len(textures),
        "texture_bytes": sum(p.stat().st_size for p in textures),
    }


def duration_ms(text):
    match = re.fullmatch(r"([\d.]+)(ns|µs|us|ms|s)", text)
    if not match:
        raise ValueError(f"Unknown engine duration: {text}")
    return float(match[1]) * {"ns": 1e-6, "µs": .001, "us": .001, "ms": 1, "s": 1000}[match[2]]


def run(binary, archive, switches, settle, output, index):
    # A short socket path also works from deeply nested checkouts.
    with tempfile.TemporaryDirectory(prefix="ob-", dir=ROOT / "target") as directory:
        runtime = Path(directory)
        env = os.environ.copy()
        for key in list(env):
            if key.startswith("OMASTORM_"):
                del env[key]
        env.update(XDG_RUNTIME_DIR=str(runtime), XDG_CACHE_HOME=str(runtime / "cache"),
                   OMASTORM_ARCHIVE=str(archive))
        log_path = output / f"run-{index}.log"
        with log_path.open("w") as log:
            started = time.perf_counter()
            engine = subprocess.Popen([str(binary), "serve"], env=env, stdout=subprocess.DEVNULL, stderr=log)
            client = socket.socket(socket.AF_UNIX)
            try:
                deadline = time.monotonic() + 30
                while True:
                    if engine.poll() is not None:
                        raise RuntimeError(f"Engine exited; see {log_path}")
                    try:
                        client.connect(str(runtime / "omastorm/engine.sock"))
                        break
                    except (FileNotFoundError, ConnectionRefusedError):
                        if time.monotonic() >= deadline:
                            raise TimeoutError("Engine startup exceeded 30s")
                        time.sleep(.002)
                client.settimeout(10)
                with client.makefile("rb") as wire:
                    def state_until(predicate):
                        until = time.monotonic() + 10
                        while time.monotonic() < until:
                            line = wire.readline()
                            if not line:
                                raise RuntimeError("Engine disconnected")
                            message = json.loads(line)
                            if message["type"] == "error":
                                raise RuntimeError(message)
                            if message["type"] == "state" and predicate(message):
                                return message
                        raise TimeoutError("Expected state did not arrive")

                    state = state_until(lambda s: s.get("frame") is not None)
                    first_ms = (time.perf_counter() - started) * 1000
                    frame = state["frame"]
                    if frame["status"] != "complete" or not (runtime / "omastorm" / frame["texture"]).is_file():
                        raise RuntimeError("First frame was not published")
                    samples = [{"phase": "archive", **resources(engine.pid, runtime)}]
                    transitions = []
                    def command(message, predicate, phase):
                        before = time.perf_counter()
                        client.sendall(json.dumps(message).encode() + b"\n")
                        state_until(predicate)
                        transitions.append({"phase": phase, "ms": (time.perf_counter() - before) * 1000})
                        samples.append({"phase": phase, **resources(engine.pid, runtime)})

                    # Fixture -> uncovered centre clears selection -> fixture.
                    # These use the real selection/publication paths without starting a poller.
                    for cycle in range(switches):
                        command({"type": "select_source", "id": "fixture-mosaic"},
                                lambda s: (s.get("selection") or {}).get("sourceId") == "fixture-mosaic"
                                and (s.get("frame") or {}).get("status") == "complete", f"mosaic-{cycle}")
                        command({"type": "view_center", "lat": -80, "lon": 0},
                                lambda s: s.get("selection") is None and s.get("frame") is None, f"clear-{cycle}")
                    time.sleep(settle)
                    samples.append({"phase": "settled", **resources(engine.pid, runtime)})
            finally:
                client.close()
                engine.terminate()
                try:
                    engine.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    engine.kill()
                    engine.wait()
        text = log_path.read_text()
        timings = re.search(r"Archive ready: decoded in (\S+), textures encoded by (\S+), published by (\S+)", text)
        if not timings:
            raise RuntimeError(f"Missing archive timings in {log_path}")
        decoded, encoded, published = map(duration_ms, timings.groups())
        return {"decode_ms": decoded, "encode_ms": encoded - decoded,
                "publish_ms": published - encoded, "first_frame_ms": first_ms,
                "peak_rss_kib": max(s["peak_rss_kib"] for s in samples),
                "source_changes": transitions, "resources": samples}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/debug/omastorm-engine")
    parser.add_argument("--archive", type=Path, default=ROOT / "data/raw/KTLX20130520_201643_V06.gz")
    parser.add_argument("--runs", type=int, default=3)
    parser.add_argument("--switches", type=int, default=10)
    parser.add_argument("--settle-seconds", type=float, default=32)
    parser.add_argument("--output", type=Path, default=ROOT / "target/bench-engine")
    args = parser.parse_args()
    if args.runs < 1 or args.switches < 1 or args.settle_seconds < 0:
        parser.error("runs/switches must be positive and settle-seconds nonnegative")
    binary, archive = args.binary.resolve(), args.archive.resolve()
    if not binary.is_file() or not archive.is_file():
        parser.error("Build the engine and extract fixtures first (mise setup)")
    args.output.mkdir(parents=True, exist_ok=True)
    report = {"schema": 1, "status": "running", "measured_at": datetime.datetime.now(datetime.timezone.utc).isoformat(),
              "harness_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(), "commit": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip(),
              "binary": str(binary), "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
              "archive": str(archive), "archive_sha256": hashlib.sha256(archive.read_bytes()).hexdigest(),
              "host": platform.platform(), "cpu": next((line.split(":", 1)[1].strip() for line in Path("/proc/cpuinfo").read_text().splitlines() if line.startswith("model name")), platform.processor() or platform.machine()), "cpu_count": os.cpu_count(),
              "cpu_identities": sorted({"; ".join(line.strip() for line in block.splitlines()
                               if line.startswith(("model name", "CPU implementer", "CPU architecture", "CPU variant", "CPU part", "CPU revision")))
                               for block in Path("/proc/cpuinfo").read_text().split("\n\n") if block.strip()}),
              "working_tree": subprocess.check_output(["git", "status", "--short"], cwd=ROOT, text=True),
              "first_frame_definition": "process spawn to first complete protocol frame with published texture",
              "peak_memory_definition": "Linux process VmHWM, not whole-system or GPU memory",
              "switches": args.switches, "settle_seconds": args.settle_seconds,
              "provider_timing": None, "runs": []}
    report_path = args.output / "baseline.json"
    report_path.write_text(json.dumps(report, indent=2) + "\n")
    for index in range(args.runs):
        try:
            result = run(binary, archive, args.switches, args.settle_seconds, args.output, index)
        except Exception as error:
            report.update(status="failed", error=str(error))
            report_path.write_text(json.dumps(report, indent=2) + "\n")
            raise
        report["runs"].append(result)
        report_path.write_text(json.dumps(report, indent=2) + "\n")
        print(f"Run {index + 1}: decode {result['decode_ms']:.2f}ms, first frame {result['first_frame_ms']:.2f}ms, peak RSS {result['peak_rss_kib']} KiB", flush=True)
    report["status"] = "complete"
    report["medians"] = {key: statistics.median(r[key] for r in report["runs"])
                         for key in ("decode_ms", "encode_ms", "publish_ms", "first_frame_ms", "peak_rss_kib")}
    report_path.write_text(json.dumps(report, indent=2) + "\n")
    print(f"Report: {args.output / 'baseline.json'}")


if __name__ == "__main__":
    main()
