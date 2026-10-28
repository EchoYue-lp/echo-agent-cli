#!/usr/bin/env python3
"""Run retained harness binaries; never invokes Cargo, services, or OS cache purge.

Use only inside a coordinator-assigned quiet measurement window. A study owns one
fixed absolute synthetic workspace so copied journal/lease identities remain legal.
"""
import argparse
import contextlib
import fcntl
import hashlib
import json
import os
from pathlib import Path
import platform
import plistlib
import re
import shutil
import subprocess
import sys
import time
import uuid


def digest(data):
    return hashlib.sha256(data).hexdigest()


def encoded(value):
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode()


def write_new(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("x", encoding="utf-8") as handle:
        json.dump(value, handle, ensure_ascii=False, indent=2)
        handle.write("\n")


def load_manifest(path):
    manifest = json.loads(path.read_text())
    if digest(encoded(manifest["protocol"])) != manifest["protocol_sha256"]:
        raise ValueError("frozen protocol identity changed")
    if digest(encoded(manifest["files"])) != manifest["dataset_sha256"]:
        raise ValueError("frozen manifest identity changed")
    return manifest


def inventory(root):
    result = []
    if root.is_symlink() or not root.is_dir():
        raise ValueError("dataset root must be a real directory")
    for path in sorted(root.rglob("*")):
        if path.is_symlink():
            raise ValueError("dataset symlinks are not accepted")
        if path.is_dir():
            continue
        if not path.is_file():
            raise ValueError("non-regular dataset entry")
        data = path.read_bytes()
        result.append({"path": path.relative_to(root).as_posix(), "bytes": len(data), "sha256": digest(data)})
    if not result:
        raise ValueError("empty dataset cannot be frozen")
    return result


def command(args, cwd=None):
    call = subprocess.run(args, cwd=cwd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False)
    return {"args": args, "exit": call.returncode, "stdout": call.stdout.decode("utf-8", "replace"),
            "stderr": call.stderr.decode("utf-8", "replace")}


def source_identity(root):
    root = root.resolve()
    listing = subprocess.check_output(["git", "-C", str(root), "ls-files", "-z", "--cached", "--others", "--exclude-standard"])
    files = []
    for name in sorted(set(os.fsdecode(part) for part in listing.split(b"\0") if part)):
        path = root / name
        if path.is_symlink():
            data = os.fsencode(os.readlink(path)); kind = "symlink-target"
        elif path.is_file():
            data = path.read_bytes(); kind = "regular"
        elif not path.exists():
            data = b""; kind = "deleted"
        else:
            raise ValueError("unexpected source entry: " + name)
        files.append({"path": name, "kind": kind, "bytes": len(data), "sha256": digest(data)})
    return {"root": str(root), "head": subprocess.check_output(["git", "-C", str(root), "rev-parse", "HEAD"], text=True).strip(),
            "status": command(["git", "-C", str(root), "status", "--short"]),
            "files": files, "sha256": digest(encoded(files)),
            "locks": {name: digest((root / name).read_bytes()) for name in
                      ["Cargo.lock", "Cargo.toml", "echo-agent-app-core/Cargo.toml",
                       "web-frontend/package-lock.json", "web-frontend/package.json"]
                      if (root / name).is_file()}}


def host_identity(workspace):
    df = command(["df", "-k", str(workspace.parent)])
    fs = {"df": df, "physical_io_bytes": "not inferred from file sizes or block-operation counts"}
    if sys.platform == "darwin":
        lines = df["stdout"].splitlines()
        if len(lines) > 1:
            device = lines[1].split()[0]
            call = subprocess.run(["diskutil", "info", "-plist", device], stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False)
            if call.returncode == 0:
                info = plistlib.loads(call.stdout)
                fs.update({key: info.get(key) for key in ["FilesystemType", "FilesystemName", "SolidState", "DeviceBlockSize"]})
    return {"platform": platform.platform(), "machine": platform.machine(), "python": platform.python_version(),
            "cpu_count": os.cpu_count(), "loadavg": os.getloadavg(), "filesystem": fs,
            "page_cache": "uncontrolled; process-cold is not OS/disk cold",
            "clock": time.get_clock_info("monotonic").__dict__}


@contextlib.contextmanager
def workspace_owner(workspace, create=False):
    if not workspace.is_absolute() or workspace == Path("/"):
        raise ValueError("an absolute dedicated workspace is required")
    workspace.parent.mkdir(parents=True, exist_ok=True)
    marker = workspace.parent / (workspace.name + ".measurement-owner.json")
    lock = workspace.parent / (workspace.name + ".measurement-owner.lock")
    with lock.open("a+b") as handle:
        fcntl.flock(handle.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
        if create:
            if marker.exists() or workspace.exists():
                raise ValueError("seed never adopts or overwrites an existing workspace")
            write_new(marker, {"workspace": str(workspace), "purpose": "synthetic performance fixtures", "owner": str(uuid.uuid4())})
        else:
            if json.loads(marker.read_text())["workspace"] != str(workspace):
                raise ValueError("workspace owner marker mismatch")
        yield


def remember_workspace(workspace):
    marker = workspace.parent / (workspace.name + ".measurement-owner.json")
    state = json.loads(marker.read_text())
    if state["workspace"] != str(workspace):
        raise ValueError("workspace ownership mismatch")
    stat = workspace.stat()
    state.update({"device": stat.st_dev, "inode": stat.st_ino,
                  "last_inventory_sha256": digest(encoded(inventory(workspace)))})
    temporary = marker.with_suffix(".next")
    with temporary.open("x", encoding="utf-8") as handle:
        json.dump(state, handle, ensure_ascii=False, indent=2); handle.write("\n")
    os.replace(temporary, marker)


def child(binary, test, workspace, environment, output, override=None, cwd=None):
    """rusage is for this child plus descendants it waits for, not per API call."""
    output.mkdir(parents=True, exist_ok=False)
    stdout = output / "stdout.log"; stderr = output / "stderr.log"
    args = override or [str(binary), test, "--exact", "--ignored", "--nocapture", "--test-threads=1"]
    env = dict(os.environ)
    env.update({"EKO_PERF_ROOT": str(workspace), "EKO_PERF_FIXED_ROOT": str(workspace),
                "EKO_DATA_DIR": str(workspace / "global-data"), "RUST_TEST_THREADS": "1"})
    env.update(environment)
    before_load = os.getloadavg(); started = time.monotonic_ns()
    with stdout.open("wb") as out, stderr.open("wb") as err:
        process = subprocess.Popen(args, cwd=cwd or workspace.parent, env=env, stdout=out, stderr=err)
        watchdog = float(env.get("EKO_PERF_WATCHDOG_SECONDS", "3600" if env.get("EKO_PERF_ACTION") == "seed" else "300"))
        try:
            while True:
                completed, status, usage = os.wait4(process.pid, os.WNOHANG)
                if completed:
                    process.returncode = os.waitstatus_to_exitcode(status)
                    break
                if (time.monotonic_ns() - started) / 1_000_000_000 > watchdog:
                    raise TimeoutError("execution watchdog; this is not a product latency threshold")
                time.sleep(0.05)
        except BaseException as failure:
            # Popen's signal methods check the original child before signaling;
            # no unrelated process group or parent application is targeted.
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill(); process.wait()
            write_new(output / "aborted.json", {"exit": process.returncode, "reason": type(failure).__name__,
                      "error": str(failure), "accepted_samples": False,
                      "scope": "child leader ended; workspace retained, no next reset or success distribution"})
            raise
    wall = time.monotonic_ns() - started
    # macOS reports bytes, Linux reports KiB. This is process peak RSS, not a
    # single operation's allocation or peak summed over repeated sample rows.
    rss_bytes = usage.ru_maxrss if sys.platform == "darwin" else usage.ru_maxrss * 1024
    receipt = {"args": args, "exit": process.returncode, "wall_including_launch_setup_and_validation_ns": str(wall),
               "user_cpu_seconds": usage.ru_utime, "system_cpu_seconds": usage.ru_stime,
               "peak_rss_bytes": rss_bytes, "scope": "child process and waited descendants; includes setup/harness",
               "input_block_operations": usage.ru_inblock, "output_block_operations": usage.ru_oublock,
               "block_io_limit": "OS operation counters; not physical byte throughput; may be zero on macOS/APFS",
               "load_before": before_load, "load_after": os.getloadavg(),
               "stdout": {"path": str(stdout.resolve()), "sha256": digest(stdout.read_bytes())},
               "stderr": {"path": str(stderr.resolve()), "sha256": digest(stderr.read_bytes())}}
    write_new(output / "process.json", receipt)
    text = stdout.read_text(errors="replace")
    if process.returncode != 0 or (override is None and not re.search(r"test result: ok\. 1 passed; 0 failed; 0 ignored; 0 measured;", text)):
        raise RuntimeError("harness did not execute exactly one successful non-measured test; inspect " + str(output))
    return receipt



NATIVE_HISTORY_CASES = frozenset({"native-history-gui", "native-history-fork", "native-history-safe-point"})


def workload_units(case):
    units = {"objects": "entries/runs/conversations only where named in fixture",
             "events": "journal records, never task count",
             "memory_history_mutations": "complete writes: two operation records and one audit entry each",
             "gui": "accepted graph nodes; depth <= 10"}
    if case in NATIVE_HISTORY_CASES:
        units.update({"native_history_messages": "eligible canonical Message values in one ordinary caller batch; never cloned rows",
                      "native_history_events": "observed ordinary ChatEventLog records; --events=0 selects actual default retention fixture, never zero records; not TaskRuntime or run count",
                      "native_history_io": "separate attempted/successful/failed logical storage operations with test-utils tracing/collector overhead; not OS syscalls, bytes or per-row wait4"})
    return units


def seed(args):
    workspace = args.workspace.resolve(); snapshot = args.snapshot.resolve()
    if workspace == snapshot or workspace in snapshot.parents or snapshot in workspace.parents:
        raise ValueError("snapshot and workspace must be disjoint")
    if snapshot.exists():
        raise ValueError("snapshot already exists; never reseed a frozen identity")
    metadata = json.loads(args.metadata.read_text())
    if digest(encoded(metadata["protocol"])) != metadata["protocol_sha256"]:
        raise ValueError("measurement protocol identity mismatch")
    qualified = metadata["source"]
    for name in ["framework", "eko"]:
        if source_identity(Path(qualified[name]["root"]))["sha256"] != qualified[name]["sha256"]:
            raise ValueError("source changed before dataset generation")
    if qualified["profiles"]["retained_binaries"].get(str(args.binary.resolve())) != digest(args.binary.read_bytes()):
        raise ValueError("seed executable differs from qualified retained binary")
    with workspace_owner(workspace, create=True):
        env = {"EKO_PERF_ACTION": "seed", "EKO_PERF_CASE": args.case, "EKO_PERF_OBJECTS": str(args.objects),
               "EKO_PERF_HISTORY": str(args.history), "EKO_PERF_EVENTS": str(args.events),
               "EKO_PERF_TAIL": str(args.tail), "EKO_PERF_CHECKPOINT": args.checkpoint}
        child(args.binary.resolve(), args.test, workspace, env, args.output / "seed")
        if args.post_steps:
            for ordinal, step in enumerate(json.loads(args.post_steps.read_text())):
                binary = Path(step["binary"]).resolve()
                if qualified["profiles"]["retained_binaries"].get(str(binary)) != digest(binary.read_bytes()):
                    raise ValueError("post-step binary differs from qualified source")
                child(binary, step["test"], workspace, {**env, "EKO_PERF_ACTION": step["action"]}, args.output / ("post-" + str(ordinal)))
        # Validation stays before freezing; runtime-history validation deliberately
        # does not repair discardable checkpoint variants before cold sampling.
        child(args.binary.resolve(), args.test, workspace, {**env, "EKO_PERF_ACTION": "validate"}, args.output / "validate")
        fixture = json.loads((workspace / "fixture.json").read_text())
        if fixture["case"] != args.case:
            raise ValueError("generation label differs from real harness fixture")
        files = inventory(workspace)
        remember_workspace(workspace)
        shutil.copytree(workspace, snapshot, copy_function=shutil.copy2)
        if inventory(snapshot) != files:
            raise RuntimeError("snapshot copy readback differs")
        manifest = {"fixed_workspace": str(workspace), "snapshot": str(snapshot), "files": files,
                    "dataset_sha256": digest(encoded(files)), "metadata": metadata,
                    "protocol": metadata["protocol"],
                    "protocol_sha256": metadata["protocol_sha256"],
                    "generation": {"case": args.case, "fixture_facts": fixture,
                                   "units": workload_units(args.case)},
                    "host": host_identity(workspace)}
        manifest["comparison"] = {"units": manifest["generation"]["units"],
                                  "features": qualified["profiles"]["features"],
                                  "profile": qualified["profiles"]["profile"],
                                  "durability": qualified["profiles"]["durability"],
                                  "toolset_sha256": digest(encoded(fixture["profiles"])) if args.case == "tool-catalog" else None}
        write_new(args.manifest, manifest)


def restore(workspace, snapshot, manifest):
    if str(workspace) != manifest["fixed_workspace"] or str(snapshot) != manifest["snapshot"]:
        raise ValueError("absolute authority root cannot be relocated")
    if inventory(snapshot) != manifest["files"] or digest(encoded(manifest["files"])) != manifest["dataset_sha256"]:
        raise ValueError("frozen byte identity changed")
    if workspace.is_symlink():
        raise ValueError("workspace path is a symlink")
    if workspace.exists():
        # Only this dedicated synthetic directory is governed by the caller's
        # exact owner lock. Child process completion precedes every reset.
        owner = json.loads((workspace.parent / (workspace.name + ".measurement-owner.json")).read_text())
        current = workspace.stat()
        if current.st_ino != owner["inode"] or current.st_dev != owner["device"]:
            raise ValueError("synthetic workspace directory was replaced; retain it")
        if digest(encoded(inventory(workspace))) != owner["last_inventory_sha256"]:
            raise ValueError("workspace changed outside its recorded child operation; retain it")
        shutil.rmtree(workspace)
    shutil.copytree(snapshot, workspace, copy_function=shutil.copy2)
    if inventory(workspace) != manifest["files"]:
        raise RuntimeError("restored input is not byte-identical")
    remember_workspace(workspace)


def observation_file(output, process_id, receipt, copy_ns, row_count):
    path = output / (process_id + ".observation.json")
    write_new(path, {"schema_version": 1, "process_observation_id": process_id,
                    "process": receipt, "input_copy_and_verification_ns_excluded_from_child": str(copy_ns),
                    "operation_row_count": row_count, "rss_cpu_io_not_per_operation": True})
    return {"path": str(path.resolve()), "sha256": digest(path.read_bytes())}


def run_receipt(args, manifest, source, host, samples, observations, kind, binary):
    """One evidence envelope; data rows and process counters keep separate scopes."""
    return {"schema_version": 1, "kind": kind, "mode": args.mode if kind == "rust" else "jsdom/react-test-development",
            "test": args.test if kind == "rust" else "TaskRuntimePanel.performance.test.tsx",
            "dataset_manifest": {"path": str(args.manifest.resolve()), "sha256": digest(args.manifest.read_bytes()),
                                 **{key: manifest[key] for key in ["dataset_sha256", "protocol_sha256", "fixed_workspace", "generation", "metadata", "comparison"]}},
            "source": {"metadata_path": str(args.source.resolve()), "metadata_sha256": digest(args.source.read_bytes()),
                       "source_sha256": source["source_sha256"], "capsule": source},
            "host": host, "environment_id": digest(encoded(host)), "binary": binary,
            "samples": {"path": str(samples.resolve()), "sha256": digest(samples.read_bytes()),
                        "rows": len(samples.read_text().splitlines())},
            "process_observations": observations,
            "boundaries": {"native_layout_paint": "not established",
                           "timing_scope": "operation-specific raw correctness.scope/timing fields; includes validation where named",
                           "resources": "one actual child/waited-descendant observation, not per-row CPU/RSS/IO",
                           "physical_io_bytes": "unavailable from wait4; block counters are operations, file sizes are logical bytes",
                           "cold": "fresh process with uncontrolled OS page cache",
                           "statistics": "descriptive; no small-N p99, confidence intervals, SLA or causal optimization claim"}}


def validate_rows(rows, manifest, source_sha, environment_id, process_id, mode, phase, index):
    if not rows:
        raise RuntimeError("missing raw operation samples")
    groups = {}
    for row in rows:
        if row.get("correctness", {}).get("ok") is not True:
            raise RuntimeError("incorrect raw operation sample")
        for key, expected in [("dataset_sha256", manifest["dataset_sha256"]), ("source_sha256", source_sha),
                              ("protocol_sha256", manifest["protocol_sha256"]), ("environment_id", environment_id),
                              ("process_observation_id", process_id)]:
            if row.get(key) != expected:
                raise RuntimeError("operation input identity mismatch: " + key)
        if row.get("outcome") not in ["success", "preview_only", "expected_refusal"]:
            raise RuntimeError("unqualified operation outcome")
        if not isinstance(row.get("elapsed_ns"), str) or not row["elapsed_ns"].isdigit():
            raise RuntimeError("duration must be a nonnegative integer string")
        key = (row["scenario"], row["mode"], row["outcome"])
        groups.setdefault(key, []).append((row["phase"], row["sample_index"]))
    for (_, row_mode, _), indices in groups.items():
        if row_mode == "diagnostic":
            expected = [("measurement", 0)]
        elif mode in ["hot", "tool-schema", "tool-output", "tool-wire", "gui"]:
            expected = [("warmup", number) for number in range(5)] + [("measurement", number) for number in range(30)]
        else:
            expected = [(phase, index)]
        if sorted(indices) != sorted(expected):
            raise RuntimeError("operation omitted/duplicated or mixed its fixed phase/sample protocol")
    return groups


def run(args):
    manifest = load_manifest(args.manifest); workspace = Path(manifest["fixed_workspace"]); snapshot = Path(manifest["snapshot"])
    if shutil.disk_usage(workspace.parent).free < 5 * 1024**3:
        raise RuntimeError("less than 5 GiB free; do not start measurement")
    if args.mode in ["process-cold", "runtime-cold", "runtime-boot", "memory-reopen"]:
        phases = [("measurement", i) for i in range(10)]
    elif args.mode in ["memory-write", "memory-app", "store-primitive"]:
        phases = [("warmup", i) for i in range(5)] + [("measurement", i) for i in range(20)]
    elif args.mode == "memory-guard":
        phases = [("measurement", 0)]  # independent refusal diagnostic, never successful-write distribution
    elif args.mode in ["hot", "tool-schema", "tool-output", "tool-wire"]:
        phases = [("in-process", 0)]  # Rust entry owns the fixed 5 + 30 loops
    else:
        raise ValueError("unknown mode")
    args.output.mkdir(parents=True, exist_ok=False)
    source = json.loads(args.source.read_text()); host = host_identity(workspace)
    for name in ["framework", "eko"]:
        current = source_identity(Path(source[name]["root"]))
        if current["sha256"] != source[name]["sha256"]:
            raise ValueError("source changed after the qualified retained build: " + name)
    retained = source["profiles"]["retained_binaries"]
    binary_sha = digest(args.binary.read_bytes())
    if retained.get(str(args.binary.resolve())) != binary_sha:
        raise ValueError("retained binary does not match the recorded build receipt")
    if workspace in args.binary.resolve().parents:
        raise ValueError("retained executable must be outside the resettable workspace")
    source_sha = source["source_sha256"]
    environment_id = digest(encoded(host))
    samples = args.output / "samples.jsonl"; observations = []
    with workspace_owner(workspace):
        for phase, index in phases:
            if shutil.disk_usage(workspace.parent).free < 5 * 1024**3:
                raise RuntimeError("disk floor reached before next restore")
            started = time.monotonic_ns(); restore(workspace, snapshot, manifest); copy_ns = time.monotonic_ns() - started
            process_id = str(uuid.uuid4()); before = len(samples.read_text().splitlines()) if samples.exists() else 0
            env = {"EKO_PERF_ACTION": "sample", "EKO_PERF_MODE": args.mode,
                   "EKO_PERF_PHASE": phase if phase != "in-process" else "measurement", "EKO_PERF_SAMPLE_INDEX": str(index),
                   "EKO_PERF_DATASET_SHA256": manifest["dataset_sha256"], "EKO_PERF_SOURCE_SHA256": source_sha,
                   "EKO_PERF_PROTOCOL_SHA256": manifest["protocol_sha256"], "EKO_PERF_ENVIRONMENT_ID": environment_id,
                   "EKO_PERF_PROCESS_ID": process_id, "EKO_PERF_SAMPLES": str(samples.resolve())}
            receipt = child(args.binary.resolve(), args.test, workspace, env, args.output / process_id)
            remember_workspace(workspace)
            rows = [json.loads(line) for line in samples.read_text().splitlines()][before:]
            observations.append(observation_file(args.output, process_id, receipt, copy_ns, len(rows)))
            validate_rows(rows, manifest, source_sha, environment_id, process_id, args.mode, phase, index)
    write_new(args.output / "run.json", run_receipt(args, manifest, source, host, samples, observations, "rust",
              {"path": str(args.binary.resolve()), "sha256": binary_sha, "retained_profile": source["profiles"]}))


def gui(args):
    manifest = load_manifest(args.manifest)
    workspace = Path(manifest["fixed_workspace"]); snapshot = Path(manifest["snapshot"])
    if shutil.disk_usage(workspace.parent).free < 5 * 1024**3:
        raise RuntimeError("less than 5 GiB free; do not start GUI observation")
    source = json.loads(args.source.read_text())
    for name in ["framework", "eko"]:
        if source_identity(Path(source[name]["root"]))["sha256"] != source[name]["sha256"]:
            raise ValueError("source changed before GUI observation")
    args.output.mkdir(parents=True, exist_ok=False)
    host = host_identity(workspace); environment_id = digest(encoded(host)); process_id = str(uuid.uuid4())
    with workspace_owner(workspace):
        started = time.monotonic_ns(); restore(workspace, snapshot, manifest); copy_ns = time.monotonic_ns() - started
        fixture = workspace / "gui.json"
        facts = json.loads(fixture.read_text())
        if facts["objects"] not in [1, 10, 50, 100]:
            raise ValueError("GUI graph outside product protocol")
        samples = args.output / "samples.jsonl"
        env = {"EKO_PERF_GUI_FIXTURE": str(fixture), "EKO_PERF_GUI_FIXTURE_SHA256": digest(fixture.read_bytes()),
               "EKO_PERF_ACTION": "sample",
               "EKO_PERF_DATASET_SHA256": manifest["dataset_sha256"], "EKO_PERF_SOURCE_SHA256": source["source_sha256"],
               "EKO_PERF_PROTOCOL_SHA256": manifest["protocol_sha256"], "EKO_PERF_ENVIRONMENT_ID": environment_id,
               "EKO_PERF_PROCESS_ID": process_id, "EKO_PERF_SAMPLES": str(samples.resolve())}
        args_list = [str(args.node.resolve()), str(args.vitest.resolve()), "run",
                     "src/components/task/TaskRuntimePanel.performance.test.tsx", "--pool=forks", "--maxWorkers=1", "--fileParallelism=false"]
        observation = child(args.node, "GUI", workspace, env, args.output / process_id, override=args_list, cwd=args.frontend.resolve())
        remember_workspace(workspace)
        rows = [json.loads(line) for line in samples.read_text().splitlines()]
        observations = [observation_file(args.output, process_id, observation, copy_ns, len(rows))]
        validate_rows(rows, manifest, source["source_sha256"], environment_id, process_id, "gui", "measurement", 0)
        groups = {}
        for row in rows:
            if row.get("correctness", {}).get("ok") is not True or row["dataset_sha256"] != manifest["dataset_sha256"]:
                raise RuntimeError("GUI input/correctness mismatch")
            key = (row["scenario"], row["phase"])
            groups.setdefault(key, []).append(row["sample_index"])
        for operation in ["mount", "single-row", "batch", "run-state"]:
            for phase, size in [("warmup", 5), ("measurement", 30)]:
                if sorted(groups.get(("gui/TaskRuntimePanel/" + operation, phase), [])) != list(range(size)):
                    raise RuntimeError("GUI did not execute its registered sample protocol")
        receipt = run_receipt(args, manifest, source, host, samples, observations, "gui",
                  {"node": {"path": str(args.node.resolve()), "sha256": digest(args.node.read_bytes())},
                   "vitest_entry": {"path": str(args.vitest.resolve()), "sha256": digest(args.vitest.read_bytes())}})
        receipt["boundaries"]["timing_scope"] = "jsdom/development React actual component commits; no browser/native layout or paint"
        write_new(args.output / "run.json", receipt)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    capture = sub.add_parser("capture")
    capture.add_argument("--framework", type=Path, required=True); capture.add_argument("--eko", type=Path, required=True)
    capture.add_argument("--output", type=Path, required=True); capture.add_argument("--profile-metadata", type=Path, required=True)
    build = sub.add_parser("seed")
    for name in ["binary", "workspace", "snapshot", "manifest", "metadata", "output"]:
        build.add_argument("--" + name, type=Path, required=True)
    build.add_argument("--test", required=True); build.add_argument("--case", required=True)
    build.add_argument("--objects", type=int, default=100); build.add_argument("--history", type=int, default=0)
    build.add_argument("--events", type=int, default=10000); build.add_argument("--tail", type=int, default=0)
    build.add_argument("--checkpoint", choices=["valid", "tail", "missing", "corrupt"], default="valid")
    build.add_argument("--post-steps", type=Path)
    measure = sub.add_parser("run")
    for name in ["binary", "manifest", "source", "output"]:
        measure.add_argument("--" + name, type=Path, required=True)
    measure.add_argument("--test", required=True); measure.add_argument("--mode", required=True)
    renderer = sub.add_parser("gui")
    for name in ["node", "vitest", "frontend", "manifest", "source", "output"]:
        renderer.add_argument("--" + name, type=Path, required=True)
    args = parser.parse_args()
    if args.command == "capture":
        f = source_identity(args.framework); e = source_identity(args.eko)
        profiles = json.loads(args.profile_metadata.read_text())
        for required in ["rustc", "cargo", "features", "profile", "durability", "compile_receipt", "retained_binaries"]:
            if not profiles.get(required):
                raise ValueError("missing actual build-profile evidence: " + required)
        receipt = Path(profiles["compile_receipt"]["path"])
        if digest(receipt.read_bytes()) != profiles["compile_receipt"]["sha256"]:
            raise ValueError("compile receipt changed")
        for path, expected in profiles["retained_binaries"].items():
            if digest(Path(path).read_bytes()) != expected:
                raise ValueError("retained build artifact changed")
        paths = []
        for manifest in [args.eko / "Cargo.toml", args.eko / "echo-agent-app-core/Cargo.toml"]:
            for raw in re.findall(r'path\s*=\s*"([^"]*echo-agent)"', manifest.read_text()):
                resolved = (manifest.parent / raw).resolve()
                if resolved != args.framework.resolve():
                    raise ValueError("consumer path resolves to a different framework checkout")
                paths.append({"manifest": str(manifest), "path": raw, "resolved": str(resolved)})
        if len(paths) < 2:
            raise ValueError("framework path dependencies not found")
        # Caller supplies actual saved cargo feature/profile commands and exits,
        # never guessed features. No credentials/environment dump is collected.
        write_new(args.output, {"framework": f, "eko": e, "profiles": profiles, "path_dependencies": paths,
                  "source_sha256": digest(encoded({"framework": f["sha256"], "eko": e["sha256"], "profiles": profiles}))})
    elif args.command == "seed": seed(args)
    elif args.command == "gui": gui(args)
    else: run(args)


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, RuntimeError, KeyError) as error:
        print(str(error), file=sys.stderr)
        sys.exit(1)
