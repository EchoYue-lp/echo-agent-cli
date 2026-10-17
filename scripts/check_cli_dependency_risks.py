"""Run unfiltered cargo-audit, then verify EKO's exact human risk decision.

This is an application policy adapter, not an advisory scanner. The original
scanner output and exit remain separate from this gate's result.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys

if sys.version_info < (3, 11):
    raise SystemExit("CLI dependency risk tooling requires Python 3.11 or newer")

import tomllib


ROOT = Path(__file__).resolve().parents[1]
POLICY_PATH = ROOT / ".cargo/accepted-dependency-risks.json"
IDENTITY_FIELDS = ("name", "version", "source", "checksum")


def reject_audit_overrides(root: Path) -> None:
    # These options are not all echoed in cargo-audit's JSON settings. An
    # unreviewed config must not silently disable fetch/yanked or change DB URL.
    home_value = os.environ.get("HOME")
    home = Path(home_value) if home_value is not None else Path.home()
    cargo_home = Path(os.environ.get("CARGO_HOME") or home / ".cargo")
    if not cargo_home.is_absolute():
        cargo_home = root / cargo_home
    for config in (root / ".cargo/audit.toml", cargo_home / "audit.toml"):
        if config.exists() or config.is_symlink():
            raise ValueError(f"unaccepted cargo-audit configuration: {config}")


def consumer_digest(root: Path) -> str:
    # Bind additions/deletions as well as bytes. A new Rust caller can expand
    # exposure, so the decision covers all application Rust consumer inputs.
    paths = {"Cargo.toml", "echo-agent-app-core/Cargo.toml", ".cargo/config.toml"}
    for directory in ("src", "echo-agent-app-core/src", "src-tauri"):
        paths.update(p.relative_to(root).as_posix() for p in (root / directory).rglob("*.rs"))
    for name in ("build.rs", "echo-agent-app-core/build.rs"):
        if (root / name).exists():
            paths.add(name)
    digest = hashlib.sha256()
    for name in sorted(paths):
        path = root / name
        if path.is_symlink() or not path.is_file():
            raise ValueError(f"invalid consumer input: {name}")
        digest.update(name.encode() + b"\0" + hashlib.sha256(path.read_bytes()).digest())
    return digest.hexdigest()


def verify_inputs(root: Path, policy: dict) -> list[dict]:
    reject_audit_overrides(root)
    if policy.get("schema_version") != 1:
        raise ValueError("unsupported accepted-risk policy")
    lock = (root / "Cargo.lock").read_bytes()
    if hashlib.sha256(lock).hexdigest() != policy["lock_sha256"]:
        raise ValueError("actual lock changed; human risk reassessment required")
    if consumer_digest(root) != policy["consumer_inputs_sha256"]:
        raise ValueError("consumer inputs changed; human risk reassessment required")
    packages = tomllib.loads(lock.decode())["package"]
    risks = policy["accepted"]
    if len(risks) != 3 or len({r["advisory"] for r in risks}) != 3:
        raise ValueError("expected three distinct human-accepted risks")
    for risk in risks:
        matches = [p for p in packages if all(p.get(k) == risk[k] for k in IDENTITY_FIELDS)]
        if len(matches) != 1:
            raise ValueError(f"accepted package identity missing or ambiguous: {risk['name']}")
    return packages


def verify_report(report: dict, scanner_exit: int, policy: dict, package_count: int) -> list[str]:
    if set(report) != {"database", "lockfile", "settings", "vulnerabilities", "warnings"}:
        raise ValueError("unexpected cargo-audit report schema")
    if not isinstance(report["database"].get("advisory-count"), int) or report["database"]["advisory-count"] <= 0:
        raise ValueError("missing advisory database")
    if report["lockfile"] != {"dependency-count": package_count}:
        raise ValueError("scanner lock population differs from actual lock")
    if report["settings"] != {
        "target_arch": [], "target_os": [], "severity": None, "ignore": [],
        "informational_warnings": ["unmaintained", "unsound", "notice"],
    }:
        raise ValueError("scanner filters/ignores or warning settings changed")
    if report["vulnerabilities"] != {"found": False, "count": 0, "list": []}:
        raise ValueError("vulnerability findings are never accepted")
    warnings = report["warnings"]
    if not isinstance(warnings, dict) or set(warnings) - {"unmaintained", "unsound", "notice"}:
        raise ValueError("unknown/yanked warning category")
    accepted = []
    for category, entries in warnings.items():
        if not isinstance(entries, list):
            raise ValueError("malformed warning entries")
        for entry in entries:
            package, advisory = entry["package"], entry["advisory"]
            matches = [r for r in policy["accepted"] if
                       r["advisory"] == advisory["id"] and r["kind"] == category and
                       all(package.get(k) == r[k] for k in IDENTITY_FIELDS)]
            if (len(matches) != 1 or entry["kind"] != category or
                    advisory["informational"] != category or advisory["package"] != package["name"] or
                    advisory.get("withdrawn") is not None or package.get("replace") is not None):
                raise ValueError(f"unaccepted warning or changed identity: {advisory.get('id')}")
            if advisory["id"] in accepted:
                raise ValueError("duplicate advisory finding")
            accepted.append(advisory["id"])
    if scanner_exit != (1 if accepted else 0):
        raise ValueError(f"scanner failed or exit/report disagree: {scanner_exit}")
    return sorted(accepted)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--report-prefix", type=Path, required=True)
    parser.add_argument("--registry-origins", action="store_true",
                        help="generate fresh view with the sibling framework's canonical helper")
    parser.add_argument("--no-fetch", action="store_true", help="local cached-DB diagnostics only")
    args = parser.parse_args()
    prefix = args.report_prefix.resolve()
    prefix.parent.mkdir(parents=True, exist_ok=True)
    result = {"gate": "failed", "scannerExit": None, "accepted": [], "noFetch": args.no_fetch}
    try:
        policy = json.loads(POLICY_PATH.read_text())
        packages = verify_inputs(ROOT, policy)
        lockfile = ROOT / "Cargo.lock"
        if args.registry_origins:
            lockfile = Path(str(prefix) + ".registry-origin.lock")
            # The canonical framework helper owns this projection; never copy
            # its parser, substitute its build lock, or edit the CLI build lock.
            helper = ROOT.parent / "echo-agent/scripts/generate-vendored-media-audit-lock.py"
            generated = subprocess.run([sys.executable, "-B", str(helper), "--lockfile",
                                        str(ROOT / "Cargo.lock"), "--output", str(lockfile)],
                                       capture_output=True)
            Path(str(prefix) + ".origin.stdout").write_bytes(generated.stdout)
            Path(str(prefix) + ".origin.stderr").write_bytes(generated.stderr)
            if generated.returncode:
                raise ValueError(f"canonical registry-origin helper failed: {generated.returncode}")
        command = ["cargo", "audit", "--json", "--deny", "warnings",
                   "--url", "https://github.com/RustSec/advisory-db.git", "--file", str(lockfile)]
        if args.no_fetch:
            command.append("--no-fetch")
        result["command"] = command
        scan = subprocess.run(command, cwd=ROOT, capture_output=True)
        result["scannerExit"] = scan.returncode
        Path(str(prefix) + ".audit.json").write_bytes(scan.stdout)
        Path(str(prefix) + ".audit.stderr").write_bytes(scan.stderr)
        # Complete official output is also retained in CI logs on rejection.
        sys.stdout.buffer.write(scan.stdout)
        sys.stderr.buffer.write(scan.stderr)
        report = json.loads(scan.stdout)
        result["accepted"] = verify_report(report, scan.returncode, policy, len(packages))
        verify_inputs(ROOT, policy)
        result["gate"] = "passed"
    except (ValueError, KeyError, TypeError, AttributeError, OSError) as error:
        result["error"] = str(error)
    Path(str(prefix) + ".gate.json").write_text(json.dumps(result, indent=2) + "\n")
    print("\nExact accepted-risk gate (separate from cargo-audit): " + json.dumps(result), flush=True)
    return 0 if result["gate"] == "passed" else 1


if __name__ == "__main__":
    raise SystemExit(main())
