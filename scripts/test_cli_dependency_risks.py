"""Regression cases for exact acceptance; no scanner or network is mocked green."""
from __future__ import annotations

import copy
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import check_cli_dependency_risks as gate


POLICY = json.loads(gate.POLICY_PATH.read_text())


def report_for(risks: list[dict]) -> dict:
    report = {
        "database": {"advisory-count": 1}, "lockfile": {"dependency-count": 839},
        "settings": {"target_arch": [], "target_os": [], "severity": None, "ignore": [],
                     "informational_warnings": ["unmaintained", "unsound", "notice"]},
        "vulnerabilities": {"found": False, "count": 0, "list": []}, "warnings": {},
    }
    for risk in risks:
        report["warnings"].setdefault(risk["kind"], []).append({
            "kind": risk["kind"], "package": {k: risk[k] for k in gate.IDENTITY_FIELDS},
            "advisory": {"id": risk["advisory"], "package": risk["name"],
                         "informational": risk["kind"], "withdrawn": None},
        })
    return report


class AcceptedRiskTests(unittest.TestCase):
    def test_exact_three_and_clean_official_results(self):
        self.assertEqual(len(gate.verify_report(report_for(POLICY["accepted"]), 1, POLICY, 839)), 3)
        self.assertEqual(gate.verify_report(report_for([]), 0, POLICY, 839), [])
        self.assertEqual(len(gate.verify_inputs(gate.ROOT, POLICY)), 839)

    def test_added_advisory_and_changed_identity_never_inherit_acceptance(self):
        for field in (*gate.IDENTITY_FIELDS, "advisory", "kind"):
            with self.subTest(field=field):
                risks = copy.deepcopy(POLICY["accepted"])
                risks[0][field] += "-changed"
                with self.assertRaises(ValueError):
                    gate.verify_report(report_for(risks), 1, POLICY, 839)
        report = report_for(POLICY["accepted"])
        extra = copy.deepcopy(report["warnings"]["unsound"][0])
        extra["advisory"]["id"] = "RUSTSEC-2099-0001"
        report["warnings"]["unsound"].append(extra)
        with self.assertRaises(ValueError):
            gate.verify_report(report, 1, POLICY, 839)

    def test_vulnerability_yank_filters_schema_and_scanner_failure_block(self):
        alterations = [
            ("vulnerabilities", {"found": True, "count": 1, "list": [{}]}),
            ("warnings", {"yanked": [{}]}),
            ("warnings", {"notice": [{}]}),
            ("settings", {"ignore": ["RUSTSEC-2024-0429"]}),
            ("database", {"advisory-count": 0}),
            ("lockfile", {"dependency-count": 838}),
        ]
        for key, value in alterations:
            with self.subTest(key=key, value=value):
                report = report_for(POLICY["accepted"])
                report[key] = value
                with self.assertRaises((ValueError, KeyError)):
                    gate.verify_report(report, 1, POLICY, 839)
        for code in (-9, 0, 2, 101):
            with self.subTest(scanner_exit=code), self.assertRaises(ValueError):
                gate.verify_report(report_for(POLICY["accepted"]), code, POLICY, 839)
        with self.assertRaises(ValueError):
            gate.verify_report({"error": "database fetch failed"}, 1, POLICY, 839)
        with self.assertRaises(ValueError):
            gate.verify_report(report_for([]), 1, POLICY, 839)

    def make_consumer(self, root: Path) -> dict:
        for name in ("src", "src-tauri", "echo-agent-app-core/src", ".cargo", "scripts"):
            (root / name).mkdir(parents=True, exist_ok=True)
        for name in ("Cargo.toml", "echo-agent-app-core/Cargo.toml", ".cargo/config.toml"):
            (root / name).write_text("# fixture\n")
        shutil.copyfile(gate.ROOT / "Cargo.lock", root / "Cargo.lock")
        policy = copy.deepcopy(POLICY)
        policy["consumer_inputs_sha256"] = gate.consumer_digest(root)
        (root / ".cargo/accepted-dependency-risks.json").write_text(json.dumps(policy))
        return policy

    def test_lock_feature_source_changes_and_new_callers_require_reassessment(self):
        for name in ("Cargo.lock", "Cargo.toml", ".cargo/config.toml", "src/new_consumer.rs",
                     "src-tauri/new_consumer.rs", "echo-agent-app-core/src/new_consumer.rs"):
            with self.subTest(name=name), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                policy = self.make_consumer(root)
                self.assertEqual(len(gate.verify_inputs(root, policy)), 839)
                path = root / name
                path.write_bytes((path.read_bytes() if path.exists() else b"") + b"\n# changed\n")
                with self.assertRaises(ValueError):
                    gate.verify_inputs(root, policy)
        changed = copy.deepcopy(POLICY)
        changed["accepted"][0]["checksum"] = "0" * 64
        with self.assertRaises(ValueError):
            gate.verify_inputs(gate.ROOT, changed)

    def test_process_adapter_retains_raw_failed_scan_and_separate_gate(self):
        # Run the real adapter with a tiny scanner process. This tests output
        # retention/error handling, not actual RustSec or network qualification.
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self.make_consumer(root)
            shutil.copyfile(Path(gate.__file__), root / "scripts/check_cli_dependency_risks.py")
            cargo = root / "cargo"
            report = report_for(POLICY["accepted"])
            for case, stdout, code in (("accepted", json.dumps(report), 1),
                                       ("fetch-error", "not JSON: offline", 1),
                                       ("crash", json.dumps(report), 101)):
                with self.subTest(case=case):
                    cargo.write_text(f"#!{sys.executable}\nimport sys\nsys.stdout.write({stdout!r})\nsys.exit({code})\n")
                    cargo.chmod(0o755)
                    prefix = root / case
                    completed = subprocess.run([sys.executable, "-B", str(root / "scripts/check_cli_dependency_risks.py"),
                                                "--report-prefix", str(prefix), "--no-fetch"],
                                               capture_output=True, env={**os.environ, "PATH": str(root) + os.pathsep + os.environ["PATH"]})
                    result = json.loads(Path(str(prefix) + ".gate.json").read_text())
                    self.assertEqual(result["scannerExit"], code)
                    self.assertEqual(Path(str(prefix) + ".audit.json").read_text(), stdout)
                    self.assertEqual(completed.returncode, 0 if case == "accepted" else 1)
                    self.assertIn(b"separate from cargo-audit", completed.stdout)

    def test_effective_audit_config_cannot_disable_fetch_or_yanked(self):
        for location in ("repository", "cargo-home", "relative-cargo-home", "broken-symlink"):
            with self.subTest(location=location), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp) / "consumer"
                policy = self.make_consumer(root)
                cargo_home = root / "cargo-home"
                cargo_home.mkdir()
                config = (root / ".cargo" if location in ("repository", "broken-symlink") else cargo_home) / "audit.toml"
                config.write_text('[database]\nfetch=false\nstale=true\nurl="https://example.invalid/db"\n'
                                  '[yanked]\nenabled=false\nupdate_index=false\n')
                if location == "broken-symlink":
                    config.unlink()
                    config.symlink_to(root / "absent-audit-config")
                cargo_home_env = "cargo-home" if location == "relative-cargo-home" else str(cargo_home)
                with patch.dict(os.environ, {"CARGO_HOME": cargo_home_env}), self.assertRaises(ValueError):
                    gate.verify_inputs(root, policy)
                shutil.copyfile(Path(gate.__file__), root / "scripts/check_cli_dependency_risks.py")
                prefix = root / "config-rejection"
                completed = subprocess.run([sys.executable, "-B", str(root / "scripts/check_cli_dependency_risks.py"),
                                            "--report-prefix", str(prefix)], capture_output=True,
                                           cwd=Path(tmp),
                                           env={**os.environ, "CARGO_HOME": cargo_home_env, "PATH": ""})
                result = json.loads(Path(str(prefix) + ".gate.json").read_text())
                self.assertEqual(completed.returncode, 1)
                self.assertIsNone(result["scannerExit"])
                self.assertIn("unaccepted cargo-audit configuration", result["error"])


if __name__ == "__main__":
    unittest.main()
