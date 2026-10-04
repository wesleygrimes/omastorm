#!/usr/bin/env python3
"""Regression checks for CI selection, especially paths that skip expensive jobs."""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import textwrap
from unittest.mock import patch
import unittest

import sys
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "scripts/tooling"))
import selection as changes


class SelectionTests(unittest.TestCase):
    def selected(self, *paths):
        return {k for k, v in changes.classify(paths).items() if v}

    def test_scenarios(self):
        cases = [
            (("README.md", "engine/README.md", "site/index.html", "branding/mark.png"), set()),
            ((".all-contributorsrc", "README.md"), set()),
            (("ui/Panel.qml",), {"ui"}),
            (("engine/src/sweep.rs",), {"engine", "ui"}),
            (("engine/src/protocol.rs", "ui/Engine.qml"), {"engine", "ui"}),
            (("Cargo.lock",), {"engine", "ui", "release"}),
            (("data/fixtures/scan.gz", "golden/scan.json"), {"engine", "ui"}),
            (("engine/release.pin",), {"pin", "ui", "installer"}),
            (("scripts/fetch-engine.sh",), {"installer", "shell"}),
            (("scripts/hooks/omastorm",), {"ui", "installer", "shell"}),
            (("run.sh",), {"installer", "shell", "ui"}),
            (("manifest.json",), {"installer", "ui"}),
            (("scripts/build-engine-release.sh",), {"engine", "ui", "release", "shell"}),
            (("ui/shaders/radar.frag",), {"ui", "rendering"}),
            (("ui/RadarMap.qml",), {"ui", "rendering"}),
            (("ui/RadarWindow.qml",), {"ui", "rendering"}),
            (("scripts/capture-demo.sh",), {"shell"}),
            (("tests/eccc-contract.py",), {"engine"}),
            (("scripts/bench-eccc.py", "scripts/smoke-eccc.py"), {"engine", "ui"}),
            (("mise.toml",), set(changes.GROUPS)),
            ((".github/workflows/engine.yml",), set(changes.GROUPS)),
            (("new-runtime-file",), set(changes.GROUPS)),
        ]
        for paths, expected in cases:
            with self.subTest(paths=paths):
                self.assertEqual(self.selected(*paths), expected)

    def test_mixed_changes_and_full_override(self):
        self.assertEqual(self.selected("ui/Panel.qml", "scripts/fetch-engine.sh", "README.md"),
                         {"ui", "installer", "shell"})
        self.assertTrue(all(changes.classify(["README.md"], full=True).values()))

    def test_diff_includes_deleted_and_renamed_paths_and_all_commits(self):
        with tempfile.TemporaryDirectory() as scratch:
            previous = os.getcwd()
            try:
                os.chdir(scratch)
                subprocess.run(["git", "init", "-q"], check=True)
                subprocess.run(["git", "config", "user.name", "CI test"], check=True)
                subprocess.run(["git", "config", "user.email", "ci@example.invalid"], check=True)
                def commit():
                    subprocess.run(["git", "add", "-A"], check=True)
                    subprocess.run(["git", "-c", "commit.gpgsign=false", "commit", "-qm", "fixture"], check=True)
                Path("engine").mkdir()
                Path("engine/old.rs").write_text("engine source\n")
                commit()
                base = changes.git("rev-parse", "HEAD").decode().strip()
                Path("engine/old.rs").rename("README.md")
                commit()
                Path("ui").mkdir()
                Path("ui/Panel.qml").write_text("Item {}\n")
                commit()
                actual_base, paths = changes.changed_paths(base, "HEAD")
                self.assertEqual(actual_base, base)
                self.assertEqual(set(paths), {"engine/old.rs", "README.md", "ui/Panel.qml"})
                self.assertTrue(changes.classify(paths)["engine"])
                actual_base, paths = changes.changed_paths("0" * 40, "HEAD")
                self.assertIsNone(actual_base)
                self.assertIn("ui/Panel.qml", paths)
            finally:
                os.chdir(previous)


class ContributorMetadataTests(unittest.TestCase):
    def test_json_validation(self):
        with tempfile.TemporaryDirectory() as scratch:
            previous = os.getcwd()
            try:
                os.chdir(scratch)
                with patch("sys.argv", ["selection.py"]), \
                     patch.object(changes, "changed_paths", return_value=("base", [".all-contributorsrc", "README.md"])), \
                     patch.object(changes.subprocess, "run"), \
                     patch.dict(os.environ, GITHUB_STEP_SUMMARY=""), patch("builtins.print"):
                    Path(".all-contributorsrc").write_text('{"contributors": []}\n')
                    changes.main()
                    report = json.loads(Path("target/ci-changes.json").read_text())
                    self.assertFalse(any(report["groups"].values()))
                    Path(".all-contributorsrc").write_text('{"contributors": [}\n')
                    with self.assertRaises(json.JSONDecodeError):
                        changes.main()
                    # Deleted metadata does not need parsing.
                    Path(".all-contributorsrc").unlink()
                    changes.main()
            finally:
                os.chdir(previous)


class RequiredGateTests(unittest.TestCase):
    def test_selected_jobs_cannot_fail_or_be_skipped(self):
        for paths in [("README.md",), ("ui/Panel.qml",), ("engine/src/main.rs",),
                      ("run.sh",), ("scripts/hooks/omastorm",), ("engine/release.pin",), ("Cargo.lock",), ("unknown",)]:
            scope = changes.classify(paths)
            enabled = changes.required_jobs(scope)
            jobs = {name: {"result": "success" if run else "skipped"} for name, run in enabled.items()}
            jobs["changes"]["outputs"] = {k: str(v).lower() for k,v in scope.items()}
            changes.gate(jobs)
            for name, run in enabled.items():
                for status in ("failure", "cancelled", "skipped"):
                    if not run and status == "skipped": continue
                    bad = {k:dict(v) for k,v in jobs.items()}
                    bad[name]["result"] = status
                    with self.subTest(paths=paths,job=name,status=status),self.assertRaises(RuntimeError):
                        changes.gate(bad)
            jobs["changes"]["outputs"] = {}
            with self.assertRaises(RuntimeError): changes.gate(jobs)
            with self.assertRaises(RuntimeError): changes.gate({})

    def test_engine_changes_require_aarch64_before_a_tag(self):
        enabled = changes.required_jobs(changes.classify(("engine/src/main.rs",)))
        self.assertTrue(enabled["native-arm"])
        self.assertFalse(enabled["native-x86"] or enabled["bundle"])
        self.assertFalse(changes.required_jobs(changes.classify(("ui/Panel.qml",)))["native-arm"])


if __name__ == "__main__": unittest.main()
