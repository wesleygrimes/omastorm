# Verification and engine baseline

## Check boundaries

Use focused commands from [CONTRIBUTING.md](../CONTRIBUTING.md#verify-and-submit)
during iteration. Complete applicable verification runs in CI; `mise check`
remains available locally. Setup prepares dependencies and the verified pin.
UI-only checks do not build Rust; unit mode starts no daemon or GPU process.
GPU verification and inspected review captures require the desktop.

Each run retains step logs and `timings.json` under ignored `target/evidence/`.
Reports identify commit/tree state and wall time. Preserve warm and first-build
measurements separately. Integration runners lock their worktree because some
scenarios retain fixed case paths. Local fixtures and a refusal proxy prevent
live-provider fallback.

## Historical evidence

The following issue #64 results describe the earlier runner and its retained
local evidence, not the current workflow or a guaranteed timing target.

### Issue #64

The initial unchanged-tree parallel run passed: formatting 0 s, Clippy 3 s,
build 7 s, Rust tests 42 s; individual UI checks ranged from 0–11 s. This
single pass did not reproduce or disprove the historical race. The focused
engine suite subsequently passed in 2 s and the protocol suite in 41 s.
One protocol lifecycle test deliberately waits 20 s before measuring texture
retirement; that cost is part of the correctness check, not compilation.

The proposed harness retires parallel UI groups, which compete for desktop
resources, while keeping Rust tests concurrent. It rejects a second runner
in the same checkout before that runner deletes shared scratch paths.
The pin installer test uses a unique directory even when invoked directly.
Offscreen UI groups do not inherit WAYLAND_DISPLAY. These are isolation
changes, not proof of the original root cause. Repeated results below provide
the evidence for retiring the old harness; do not describe a passing rerun as
reproduction of the historical failure. Live-provider requests in some UI
checks remain a separate source of variability.

### Repeated candidate results

The focused UI suite passed in 67 s. Two unchanged complete candidate runs
passed in 68 s each, including location, keyboard and pin installation.
The overlap guard was also exercised: a second runner was rejected while
protocol checks continued successfully. The GPU rendering suite initially
failed reproducibly because its scratch harness omitted RadarMap's imported
`Metar.js`; after adding that dependency, both rendering tests passed in
46 s including build.

Timing evidence is retained under `target/verification-evidence/` as
`engine.tsv`, `protocol.tsv`, `ui.tsv`, `rendering.tsv`, `full-1.tsv` and
`full-2.tsv`. UI checks account for about 65 s of the complete run; the
42-second Rust suite overlaps them. The original full runner overlapped
both UI groups, so retiring that layout increases full-gate elapsed time.
The improvement to iteration is the explicit 2-second engine check and
ability to run only the applicable boundary. This evidence supports the
isolation proposal, but does not establish that every historical #64
failure shares one cause. Keep #64 open until the proposal is reviewed and
landed; re-open investigation with preserved logs if a serial run flakes.

## Timeline extraction order

PR #80 by Yani3rt changes loop-start semantics and currently conflicts with
main. The extraction is deferred until it lands, by maintainer decision.
Issue #79 proposed an optional shortcut; #80 changes ordinary Play, so that
product choice must be settled before landing.

After #80 lands, branch from its merged main and move `Timeline`, its playback
constants, and the tests that exercise only Timeline into
`engine/src/timeline.rs`. Keep catalog entries and protocol types as existing
dependencies, exposing only what the engine needs with `pub(crate)`.
Keep Shared/client/socket tests at their current boundary. Make no algorithm,
field, serialization, polling, or scheduling change in the move. Run engine
and protocol checks, then the complete suite. Review the move separately from
verification tooling and performance work. This avoids asking #80's author
to resolve conflicts created by an extraction.

## Recorded-input performance

```sh
mise build
mise bench
# Compare optimized code separately, with the same fixture and run parameters:
mise exec -- cargo build --release --offline --locked
mise bench recorded --binary target/release/omastorm-engine --output target/bench-engine-release
```

The baseline writes JSON and per-run engine stderr under `target/bench-engine/`.
It records commit, working-tree status, executable and archive SHA256, host,
CPU, run parameters, each sample, and medians. Run it without overlapping
builds or verification jobs. The default is three runs, each with a fresh
cache/runtime and ten source-selection cycles, then 32 seconds for the
30-second texture grace period plus cleanup ticks. The filesystem page cache
is not cleared; these are fresh application caches, not cold-disk measurements.
Python 3 and Linux `/proc` are required.

Measurements:

- Decode time: the engine's existing archive decode timing, with its logged
  precision. Encoding and publication times subtract consecutive cumulative
  timings; small rounding error is possible.
- First frame: process spawn to receipt of the first complete protocol frame
  whose texture exists. Includes executable fingerprinting, local file I/O,
  decode, encoding, catalog startup, and handshake. Excludes build and UI/GPU
  presentation time. Socket polling adds up to about 2 ms.
- Peak memory: Linux process VmHWM in KiB, including startup allocations.
  Resident memory, descriptors, threads, texture count and bytes are sampled
  at archive startup, each select/clear transition, and after retirement.
- Source changes: archive -> synthetic fixture mosaic -> uncovered view
  (clear selection), repeated. Command-to-state timings exercise engine
  selection, publication and cleanup without provider requests. This does
  not measure live poller cancellation, late network events, a realistic
  OPERA raster, or multiple recorded NEXRAD stations. Those require additional
  recorded inputs and replay adapters before comparing architectures.

The recorded KTLX input uses the provenance and golden decoder contract in
`data/README.md` and `golden/ktlx-20130520/`. The mosaic is the engine's existing
16×16 classified fixture, not a recorded provider image. No network timing
is included; `provider_timing` is null. Keep any future live-provider report
separate, naming provider, observation time, and fetch/discovery latency.

Do not use these timings as a flaky pass/fail threshold. Compare distributions
on the same host, build profile, input hashes, and run parameters. Evaluate
resource growth after retirement as well as peak allocations. Measurements
should narrow the next investigation; they do not by themselves justify
replacing the engine.

## Measured engine baseline (2026-09-29)

On Linux aarch64 (12 logical CPUs), Rust 1.98.1, engine commit
`c6cdca5bd5028daa8c9837757228108723fdb013`, using archive SHA256
`772e01b154a5c966982a6d0aa2fc78bc64f08a9b77165b74dc02d7aa5aa69275`:

| Metric (median of 3 runs) | Debug | Release |
| --- | ---: | ---: |
| Decode | 75.98 ms | 77.11 ms |
| Encode | 99.82 ms | 7.04 ms |
| Publish | 3.48 ms | 3.46 ms |
| First frame | 747.51 ms | 125.24 ms |
| Peak RSS | 146.25 MiB | 144.50 MiB |

All six runs retained 12 descriptors and one thread across the selection
cycles. All retired textures cleared after settling. The first-frame ranges
were 727.5–772.0 ms / 119.5–137.5 ms (debug / release).
Raw reports remain in `target/bench-engine/baseline.json` and
`target/bench-engine-release/baseline.json`.

The roughly 6× first-frame difference is a development-build concern. Debug
encoding accounts for about 100 ms versus 7 ms in release; the remaining
startup gap includes fingerprinting a 130 MiB debug executable, registry/cache
setup and I/O. This is an inference, not a measured attribution of that gap.
The next bounded investigation should measure fingerprinting and initialization
separately and profile decoder peak allocations (about 145 MiB in both builds).
Keep the golden decoder and protocol contract fixed. This small workload
provides no evidence supporting an engine replacement; collect multi-station
and realistic mosaic replays before making that decision.

## Tooling cleanup measurements (2026-10-01)

On the isolated main worktree at `42e6909`, first-build engine verification
spent 36 seconds in Clippy and 39 seconds building test binaries (77 seconds
wall). Sandbox socket restrictions prevented eight local HTTP-fixture tests;
with socket permission, the warm engine check passed in 2.13 seconds, including
121 Rust unit tests in 1.47 seconds. CPU rendering math is a separate fast
suite. These are measurements for this branch, not engine changes elsewhere.

The migrated runner records commands, exact tree state, per-step times and
wall time under ignored `target/evidence/`. It uses explicit published-pin
binaries for UI, without compiling or replacing a candidate. Focused migrated
transport/handoff/site/IP cases passed in 11.3 seconds; popover/reconnect/map
views passed in 20.0 seconds. The map views now use a local TileJSON endpoint
and checked-in recorded vector bytes, not a live map provider. These are
headless integration timings; desktop reload and GPU fidelity need their
own checks. Do not sum overlapping historical step times as wall time.

All 15 migrated UI/installer cases passed with the committed published pin in
50.78 seconds. Consolidated warm units passed in 2.56 seconds (engine 1.66,
CPU rendering math 0.16, UI logic 0.07, tooling 0.66). The unchanged protocol
suite passed in 41.19 seconds, preserving its real 20-second wait and retirement
assertion. These results support focused feedback under five seconds; they
are not cross-machine pass/fail budgets or proof of desktop GPU fidelity.
