#!/usr/bin/env python3
"""Offline evidence reader. It never runs a benchmark or computes percentiles.

Only explicit --output-json/--output-markdown paths are write destinations.
All referenced paths are read relative to the document containing the reference.
Exit 1 means invalid evidence; exit 2 means valid but incomplete evidence.
The adapter follows the runner's supplied schema-only example and Node
summarizeSamples contract. Examples are rejected as measurements.
"""
from __future__ import annotations

import argparse
from collections import Counter
import hashlib
import importlib.util
import json
import math
from pathlib import Path
import re
import sys


class InvalidEvidence(ValueError):
    """A supplied measurement cannot support a report."""


SAMPLE_FIELDS = {
    "scenario", "mode", "phase", "sample_index", "elapsed_ns", "outcome",
    "dataset_sha256", "source_sha256", "protocol_sha256", "environment_id",
    "process_observation_id", "correctness",
}
GUI_OBSERVATION_FIELDS = {
    "profiler_commits", "dom_elements", "clock", "viewport", "theme",
    "subagent_history", "native_layout_paint", "cpu_rss_io", "mock_boundary",
}
OUTCOMES = {"success", "preview_only", "expected_refusal"}
IDENTITY_FIELDS = ("units", "features", "profile", "durability", "toolset_sha256")
GROUP_FIELDS = ("scenario", "mode", "outcome", "dataset_sha256", "source_sha256",
                "protocol_sha256", "environment_id")
GUI_SCOPE_FIELDS = ("theme", "viewport", "native_layout_paint", "mock_boundary",
                    "clock", "subagent_history", "cpu_rss_io")
MAX_SAFE_INTEGER = 9007199254740991


def require(condition, message):
    if not condition:
        raise InvalidEvidence(message)


def digest(value):
    require(isinstance(value, str), "SHA256 must be a string")
    value = value.removeprefix("sha256:")
    require(re.fullmatch(r"[0-9a-f]{64}", value) is not None, "invalid SHA256")
    return value


def sha256_file(path):
    result = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            result.update(block)
    return result.hexdigest()


def canonical_sha256(value):
    encoded = json.dumps(value, sort_keys=True, ensure_ascii=False,
                         separators=(",", ":"), allow_nan=False).encode("utf-8")
    return hashlib.sha256(encoded).hexdigest()


def schema_example(value):
    if isinstance(value, dict):
        return (any(value.get(key) is True for key in ("schema_example_only", "schema_only", "not_a_measurement"))
                or any(schema_example(child) for child in value.values()))
    if isinstance(value, list):
        return any(schema_example(child) for child in value)
    return isinstance(value, str) and "EXAMPLE_ONLY" in value


def no_duplicate_keys(pairs):
    result = {}
    for key, value in pairs:
        require(key not in result, "duplicate JSON key: " + key)
        result[key] = value
    return result


def decode_json(text):
    def reject_constant(value):
        raise InvalidEvidence("non-finite JSON number: " + value)
    return json.loads(text, object_pairs_hook=no_duplicate_keys,
                      parse_constant=reject_constant)


def document(path, inputs):
    path = Path(path).resolve()
    inputs.add(path)
    result = decode_json(path.read_text(encoding="utf-8"))
    require(isinstance(result, dict), "JSON document must be an object: " + str(path))
    return path, result


def referenced(parent, value):
    require(isinstance(value, str) and bool(value), "reference path must be nonempty")
    path = Path(value)
    return (path if path.is_absolute() else parent.parent / path).resolve()


def bound_file(parent, descriptor, inputs):
    require(isinstance(descriptor, dict), "file descriptor must be an object")
    require("path" in descriptor and "sha256" in descriptor, "file descriptor missing path/SHA256")
    path = referenced(parent, descriptor["path"])
    inputs.add(path)
    actual = sha256_file(path)
    require(actual == digest(descriptor["sha256"]), "byte hash mismatch: " + str(path))
    return path, actual


def nonempty(value, name):
    require(isinstance(value, str) and bool(value.strip()), name + " must be nonempty")
    return value


def correctness_ok(value):
    # Deliberately do not treat arbitrary nonempty objects as a passing oracle.
    if isinstance(value, dict) and type(value.get("ok")) is bool:
        return value["ok"]
    raise InvalidEvidence("correctness has no recognized explicit boolean verdict")


def sample_key(row):
    return tuple(row[key] for key in GROUP_FIELDS)


def strip_unqualified_p99(value, count):
    """Retain upstream summaries, suppressing all p99 fields for N < 100."""
    if isinstance(value, dict):
        return {key: strip_unqualified_p99(item, count)
                for key, item in value.items()
                if count >= 100 or not re.search(r"p99|percentile_?99", key, re.I)}
    if isinstance(value, list):
        return [strip_unqualified_p99(item, count) for item in value]
    return value


def summary_groups(summary, counts):
    groups = summary.get("groups")
    require(isinstance(groups, list), "summary groups must be an array")
    result = []
    seen = set()
    for group in groups:
        require(isinstance(group, dict), "summary group must be an object")
        key = sample_key(group)
        require(key in counts and key not in seen, "summary group missing, duplicated, or unbound")
        measured, warmup = counts[key]
        require(type(group.get("n")) is int and group["n"] == measured and measured > 0,
                "summary group count differs from actual samples")
        require(type(group.get("warmup_n")) is int and group["warmup_n"] == warmup,
                "summary warmup count differs from actual samples")
        require(group.get("unit") == "ns", "summary unit is not ns")
        for field in ("min", "median", "max"):
            value = group.get(field)
            require(type(value) in (int, float) and math.isfinite(value) and 0 <= value <= MAX_SAFE_INTEGER,
                    "invalid supplied summary statistic: " + field)
        require(group["min"] <= group["median"] <= group["max"], "summary statistic ordering mismatch")
        require(measured >= 100 or "p99" not in group, "summary reports p99 for N < 100")
        for field in ("p90", "p95", "p99"):
            if field in group:
                value = group[field]
                require(type(value) in (int, float) and math.isfinite(value) and
                        group["min"] <= value <= group["max"], "invalid supplied percentile: " + field)
                require(field == "p99" or measured >= 20, "summary reports small-N p90/p95")
        seen.add(key)
        result.append({"identity": dict(zip(GROUP_FIELDS, key)),
                       "phase": "measurement", "sample_count": measured, "warmup_count": warmup,
                       "statistics_from_supplied_node_summary": strip_unqualified_p99(group, measured),
                       "p99_status": ("upstream_only" if "p99" in group else "not_present_in_supplied_summary")
                       if measured >= 100 else "not_reported_n_lt_100"})
    require(seen == set(counts), "summary does not cover every actual sample group")
    return result


def comparison_identity(run, metadata, manifest_path, inputs):
    identity = run["dataset_manifest"].get("comparison")
    require(isinstance(identity, dict), "dataset_manifest.comparison missing")
    require(identity == metadata.get("comparison"), "comparison differs from bound manifest")
    require(all(key in identity for key in IDENTITY_FIELDS), "comparison identity fields missing")
    profiles = run["source"]["capsule"].get("profiles")
    require(isinstance(profiles, dict), "source capsule profiles missing")
    for field in ("features", "profile", "durability"):
        require(field in profiles and bool(profiles[field]) and identity[field] == profiles[field],
                "comparison differs from source capsule: " + field)
    generation = metadata.get("generation")
    require(isinstance(generation, dict) and "units" in generation,
            "dataset manifest generation.units missing")
    require(identity["units"] == generation["units"], "comparison units differ from manifest")
    require(isinstance(identity["units"], dict) and bool(identity["units"]), "units must be a named object")
    if generation.get("case") == "tool-catalog":
        fixture = generation.get("fixture_facts")
        require(isinstance(fixture, dict) and "profiles" in fixture, "tool-catalog fixture profiles missing")
        snapshot = referenced(manifest_path, metadata.get("snapshot"))
        fixture_path = snapshot / "fixture.json"
        matches = [record for record in metadata["files"] if record.get("path") == "fixture.json"]
        require(len(matches) == 1, "tool-catalog frozen fixture file identity missing")
        bound_file(manifest_path, {"path": str(fixture_path), "sha256": matches[0]["sha256"]}, inputs)
        _, actual_fixture = document(fixture_path, inputs)
        require(fixture_path.stat().st_size == matches[0]["bytes"] and actual_fixture == fixture,
                "tool-catalog fixture facts differ from actual frozen fixture")
        require(digest(identity["toolset_sha256"]) == canonical_sha256(fixture["profiles"]),
                "toolset SHA differs from actual fixture profiles")
    else:
        require(identity["toolset_sha256"] is None, "non-tool-catalog toolset SHA must be null")
    return {key: identity[key] for key in IDENTITY_FIELDS}, []


def load_observations(run_path, descriptors, rows, inputs):
    require(isinstance(descriptors, list) and bool(descriptors), "process observations missing")
    actual_counts = Counter(row["process_observation_id"] for row in rows)
    seen = set()
    result = []
    for descriptor in descriptors:
        path, actual = bound_file(run_path, descriptor, inputs)
        _, observation = document(path, inputs)
        require(type(observation.get("schema_version")) is int and observation["schema_version"] == 1,
                "process schema_version must be 1")
        for field in ("process_observation_id", "process", "operation_row_count",
                      "input_copy_and_verification_ns_excluded_from_child", "rss_cpu_io_not_per_operation"):
            require(field in observation, "process observation missing field: " + field)
        identifier = nonempty(observation["process_observation_id"], "process_observation_id")
        require(identifier not in seen, "duplicate process observation id")
        seen.add(identifier)
        require(type(observation["operation_row_count"]) is int and
                observation["operation_row_count"] == actual_counts[identifier],
                "process operation row count mismatch")
        require(observation["rss_cpu_io_not_per_operation"] is True,
                "CPU/RSS/IO must remain process scoped")
        process = observation["process"]
        require(isinstance(process, dict), "process must be an object")
        for field in ("args", "exit", "wall_including_launch_setup_and_validation_ns",
                      "user_cpu_seconds", "system_cpu_seconds", "peak_rss_bytes", "scope",
                      "input_block_operations", "output_block_operations", "block_io_limit",
                      "load_before", "load_after", "stdout", "stderr"):
            require(field in process, "process missing field: " + field)
        nonempty(process["scope"], "process scope")
        require(isinstance(process["args"], list) and all(isinstance(x, str) for x in process["args"]),
                "process args must be strings")
        for field in ("user_cpu_seconds", "system_cpu_seconds"):
            value = process[field]
            require(type(value) in (int, float) and math.isfinite(value) and value >= 0,
                    "invalid process CPU value: " + field)
        for field in ("peak_rss_bytes", "input_block_operations", "output_block_operations"):
            require(type(process[field]) is int and process[field] >= 0,
                    "invalid process count: " + field)
        require(process["exit"] is None or type(process["exit"]) is int, "invalid process exit")
        for value in (process["wall_including_launch_setup_and_validation_ns"],
                      observation["input_copy_and_verification_ns_excluded_from_child"]):
            require(isinstance(value, str) and re.fullmatch(r"[0-9]+", value) is not None,
                    "process timing must be a decimal nanosecond string")
        for stream in ("stdout", "stderr"):
            bound_file(path, process[stream], inputs)
        result.append({"path": str(path), "sha256": actual,
                       "scope": "process_only_no_per_operation_allocation",
                       "rusage_units_note": "block operation counts are not physical byte throughput",
                       "observation": observation})
    require(set(actual_counts) <= seen, "sample process id has no bound observation")
    return result


def cargo_arguments(command):
    """Decode only the observed direct cargo or env NAME=value prefix; no execution."""
    if not isinstance(command, list) or not command or not all(isinstance(arg, str) for arg in command):
        return None
    position = 0
    if Path(command[0]).name == "env":
        position = 1
        while position < len(command) and re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*=.*", command[position], re.S):
            name, value = command[position].split("=", 1)
            if name not in {"CARGO_BUILD_JOBS", "CARGO_PROFILE_RELEASE_DEBUG", "CARGO_PROFILE_DEV_DEBUG",
                            "CARGO_PROFILE_TEST_DEBUG", "CARGO_TARGET_DIR", "RUST_TEST_THREADS", "TS_RS_EXPORT_DIR",
                            "CARGO_INCREMENTAL", "TMPDIR"}:
                return None
            if name == "CARGO_INCREMENTAL" and value != "0":
                return None
            if name == "TMPDIR" and not Path(value).is_absolute():
                return None
            position += 1
    if position >= len(command) or Path(command[position]).name != "cargo":
        return None
    return command[position:]


def cargo_selection(args):
    """Exact argv selection, not an inferred transitive feature closure."""
    if len(args) < 2 or args[1] != "test":
        return None, "unsupported_cargo_subcommand"
    explicit = set(); no_default = False; all_features = False; profiles = []
    position = 2
    # Only selectors used by the qualified local commands are recognized.
    # Unbound manifests/targets/configs/package aliases cannot be discarded:
    # cwd alone would not prove which checkout or target was actually built.
    values = {"-p", "--test"}
    switches = {"--locked", "--lib", "--no-run"}
    while position < len(args):
        option = args[position]
        if option in ("--features", "-F", "--profile") or option in values:
            if position + 1 >= len(args):
                return None, "missing_cargo_option_value"
            value = args[position + 1]; position += 2
        elif option.startswith("--features=") or option.startswith("--profile="):
            option, value = option.split("=", 1); position += 1
        elif option.startswith("-F") and len(option) > 2:
            value = option[2:]; option = "-F"; position += 1
        elif option == "--release":
            profiles.append("release"); position += 1; continue
        elif option == "--no-default-features":
            no_default = True; position += 1; continue
        elif option == "--all-features":
            all_features = True; position += 1; continue
        elif option in switches:
            position += 1; continue
        else:
            return None, "unsupported_cargo_option:" + option
        if option in ("--features", "-F"):
            tokens = [token for token in re.split(r"[,\s]+", value.strip()) if token]
            if not tokens or any(re.fullmatch(r"[A-Za-z0-9_][A-Za-z0-9_+./?-]*", token) is None for token in tokens):
                return None, "unsupported_feature_selection"
            explicit.update(tokens)
        elif option == "--profile":
            if not re.fullmatch(r"[A-Za-z0-9_-]+", value):
                return None, "unsupported_cargo_profile"
            profiles.append(value)
        elif option == "--test" and value != "performance_baseline":
            return None, "unbound_cargo_test_target"
    if len(profiles) > 1:
        return None, "ambiguous_cargo_profile_flags"
    if all_features and (no_default or explicit):
        return None, "unsupported_all_features_combination"
    return {"profile": profiles[0] if profiles else "test", "features": {
        "default_features": not no_default, "all_features": all_features,
        "explicit": sorted(explicit)}}, None


def snapshot_materials(source_path, profiles, roots, inputs, source_records=None):
    """复用同一个source_identity；原caller/cwd合同不因快照来源而放宽。"""
    supplied = profiles.get("source_inputs", {})
    require(isinstance(supplied, dict) and set(supplied) <= {"framework", "eko"}, "invalid source input map")
    if source_records:
        for name, record in source_records.items():
            if record.get("source_kind") == "git_object_snapshot":
                require(name in supplied and supplied[name] == record.get("source_input"), "snapshot input missing or differs from capsule")
    if not supplied:
        return {}
    # 工程入口显式绑定真实reader与同目录runner，不从证据路径加载可替换代码。
    reader, _ = bound_file(source_path, profiles.get("evidence_reader"), inputs)
    implementation, _ = bound_file(source_path, profiles.get("source_identity_reader"), inputs)
    require(reader == Path(__file__).resolve() and implementation == Path(__file__).with_name("performance-baseline-runner.py").resolve(),
            "source identity/engineering reader path differs from actual tools")
    spec = importlib.util.spec_from_file_location("bound_source_identity_reader", implementation)
    runner = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(runner)
    materials = {}
    for name, source_input in supplied.items():
        require(isinstance(source_input, dict) and source_input.get("repository") == name, "snapshot repository mismatch")
        for field in ("manifest", "git_objects", "compile_start", "sourceguard"):
            bound_file(source_path, source_input.get(field), inputs)
        if name == "framework":
            phase, _ = bound_file(source_path, source_input.get("permission_phase"), inputs)
            _, phase_doc = document(phase, inputs)
            bound_file(phase, phase_doc.get("prior_permission_phase"), inputs)
        identity = runner.source_identity(Path(roots[name]), source_input)
        if source_records:
            require(identity == source_records[name], "snapshot source record differs from actual material identity")
        materials[name] = {"source_kind": identity["source_kind"], "root": identity["root"],
                           "origin_commit": identity["origin_commit"], "head": identity["head"],
                           "sha256": identity["sha256"], "source_input": source_input}
    return materials


def compile_qualification(source_path, descriptor, inputs, profiles, roots, binary_path=None, binary_sha=None, source_records=None):
    """Qualify the explicitly bound final compile step, preserving historical attempts."""
    path, actual = bound_file(source_path, descriptor, inputs)
    qualification = {"path": str(path), "sha256": actual, "status": "unrecognized", "steps": []}
    materials = snapshot_materials(source_path, profiles, roots, inputs, source_records)
    if materials:
        qualification["source_materials"] = materials
    final_steps = descriptor.get("final_steps")
    if not isinstance(final_steps, list) or not final_steps:
        return {**qualification, "gap": "compile_final_step_descriptor_missing"}
    selected = []
    for final in final_steps:
        require(isinstance(final, dict), "compile final descriptor must be an object")
        binaries = final.get("retained_binaries")
        require(isinstance(binaries, dict) and bool(binaries), "compile final descriptor binary binding missing")
        if binary_path is None or str(binary_path) in binaries:
            selected.append(final)
    if binary_path is not None:
        require(len(selected) <= 1, "ambiguous final compile descriptor for retained binary")
        if not selected:
            return {**qualification, "gap": "compile_final_descriptor_does_not_bind_retained_binary"}
    if not selected:
        return {**qualification, "gap": "compile_final_step_descriptor_missing"}
    identities = set()
    for final in selected:
        for field in ("receipt", "label", "command", "cwd", "repository", "profile", "features", "retained_binaries"):
            if field not in final:
                return {**qualification, "gap": "compile_final_descriptor_missing:" + field}
        receipt_path, receipt_sha = bound_file(source_path, final["receipt"], inputs)
        _, evidence = document(receipt_path, inputs)
        if isinstance(evidence.get("command"), list):
            steps = [evidence]
        elif evidence.get("schemaVersion") == 1 and isinstance(evidence.get("steps"), list):
            steps = evidence["steps"]
        else:
            return {**qualification, "gap": "compile_receipt_schema_not_recognized"}
        label = nonempty(final["label"], "compile final label")
        identity = (str(receipt_path), label)
        require(identity not in identities, "duplicate final compile step descriptor")
        identities.add(identity)
        matches = [step for step in steps if isinstance(step, dict) and step.get("label") == label]
        require(len(matches) <= 1, "ambiguous final compile label in original receipt")
        if not matches:
            return {**qualification, "gap": "compile_final_step_not_found"}
        step = matches[0]
        args = cargo_arguments(step.get("command"))
        if args is None or "test" not in args or "--no-run" not in args:
            return {**qualification, "gap": "compile_final_command_wrapper_not_recognized"}
        require(step["command"] == final["command"] and step.get("cwd") == final["cwd"],
                "final compile command/cwd differ from explicit expected identity")
        require(final["repository"] in ("framework", "eko") and final["cwd"] == roots[final["repository"]],
                "final compile cwd differs from qualified repository")
        if final["repository"] in materials:
            supplied = materials[final["repository"]]["source_input"]
            started, _ = bound_file(source_path, supplied["compile_start"], inputs)
            _, original_start = document(started, inputs)
            require(original_start.get("command") == step["command"] and original_start.get("cwd") == step["cwd"],
                    "snapshot source context differs from original compile caller")
            artifacts_path, _ = bound_file(source_path, supplied.get("artifacts"), inputs)
            _, artifacts = document(artifacts_path, inputs)
            original_receipt = artifacts.get("actual_NoRun_receipt", {})
            require(referenced(artifacts_path, original_receipt.get("path")) == receipt_path and
                    digest(original_receipt.get("sha256")) == receipt_sha, "snapshot artifacts differ from actual compile receipt")
            for retained_path, retained_sha in final["retained_binaries"].items():
                matches = [r for r in artifacts.get("roles", []) if r.get("retained_binary", {}).get("path") == retained_path]
                require(len(matches) == 1 and matches[0]["retained_binary"]["sha256"] == retained_sha,
                        "snapshot binary role/source binding missing or ambiguous")
                role = matches[0]
                require(role.get("source_commit") == materials[final["repository"]]["origin_commit"] and
                        role.get("source_manifest", {}).get("sha256") == supplied["manifest"]["sha256"] and
                        role.get("original_compile_receipt", {}).get("sha256") == receipt_sha,
                        "snapshot binary source provenance differs")
                fp_path, _ = bound_file(artifacts_path, role["retained_fingerprint"], inputs)
                _, fp = document(fp_path, inputs)
                require(fp.get("profile") == role.get("actual_Cargo_profile_ID") and
                        fp.get("features") == role.get("actual_features_string"), "snapshot artifact FP/profile/features mismatch")
        selection, selection_gap = cargo_selection(args)
        if selection_gap:
            return {**qualification, "gap": selection_gap}
        require(final["profile"] == profiles["profile"] == selection["profile"],
                "final compile profile differs from actual Cargo argv")
        repository_features = profiles["features"]
        require(isinstance(repository_features, dict) and final["features"] == repository_features and
                repository_features.get(final["repository"]) == selection["features"],
                "final compile features differ from actual repository Cargo argv")
        packages = [args[index + 1] for index in range(len(args) - 1) if args[index] == "-p"]
        if final["repository"] == "framework":
            require(packages == ["echo_agent"], "final compile package differs from qualified repository")
        else:
            require(packages in (["echo-agent-app-core"], ["echo-agent-cli"]),
                    "final compile package differs from qualified repository")
            if packages == ["echo-agent-cli"]:
                require("--lib" in args and "--locked" in args and "--test" not in args and selection["features"] == {
                    "default_features": False, "all_features": False, "explicit": ["gui"]},
                    "EKO GUI history compile must select its exact no-default gui lib")
        prefix = step["command"][:-len(args)]
        temporary_roots = [arg.split("=", 1)[1] for arg in prefix if arg.startswith("TMPDIR=")]
        require(len(temporary_roots) <= 1, "ambiguous compile TMPDIR binding")
        if temporary_roots:
            workspace = Path(roots["eko"]).resolve().parent
            require(workspace in Path(temporary_roots[0]).resolve().parents,
                    "compile TMPDIR is outside the qualified shared workspace")
        for retained_path, retained_sha in final["retained_binaries"].items():
            require(profiles["retained_binaries"].get(retained_path) == retained_sha,
                    "final compile retained binary differs from qualified binary inventory")
        if binary_path is not None:
            require(digest(final["retained_binaries"][str(binary_path)]) == binary_sha,
                    "final compile descriptor does not bind this retained binary SHA")
        require(step.get("exitCode") == 0 and type(step.get("exitCode")) is int,
                "final compile receipt records failed/aborted compilation")
        require(step.get("signal") is None and step.get("errorCode") is None,
                "final compile receipt records signal or launch error")
        required = {"label", "command", "cwd", "logPath", "exitCode", "signal", "errorCode",
                    "truncated", "bytesSeen", "bytesWritten", "startedAt", "finishedAt"}
        if not required <= set(step):
            return {**qualification, "gap": "compile_receipt_missing_original_fields"}
        require(step["truncated"] is False, "final compile receipt output was truncated")
        for key in ("label", "cwd", "logPath", "startedAt", "finishedAt"):
            nonempty(step[key], "compile receipt " + key)
        require(step["finishedAt"] >= step["startedAt"], "compile receipt completion precedes start")
        require(type(step["bytesSeen"]) is int and type(step["bytesWritten"]) is int and
                step["bytesSeen"] == step["bytesWritten"] and step["bytesSeen"] >= 0,
                "compile receipt output is incomplete")
        qualification["steps"].append({"descriptor": final, "receipt_sha256": receipt_sha, "step": step,
                                       "other_original_steps": [item for item in steps if item is not step]})
    if binary_path is None and {step["descriptor"]["repository"] for step in qualification["steps"]} != {"framework", "eko"}:
        return {**qualification, "gap": "GUI_compile_repository_coverage_incomplete"}
    return {**qualification, "status": "recognized_success", "gap": None}


def gui_scope(rows, kind):
    if kind != "gui":
        return {"identities": [], "missing": []}, []
    identities = []
    missing = set()
    for row in rows:
        missing.update(key for key in GUI_SCOPE_FIELDS if key not in row)
        identity = {key: row[key] for key in GUI_SCOPE_FIELDS if key in row}
        if identity not in identities:
            identities.append(identity)
    gaps = ["GUI_scope_field_missing:" + key for key in sorted(missing)]
    if len(identities) != 1:
        gaps.append("GUI_scope_changes_within_sample_distribution")
    return {"identities": identities, "missing": sorted(missing)}, gaps


def load_run(run_path, summary_path, inputs=None):
    inputs = inputs if inputs is not None else set()
    run_path, run = document(run_path, inputs)
    require(not schema_example(run), "schema_only / EXAMPLE_ONLY evidence is not measurement data")
    require(type(run.get("schema_version")) is int and run["schema_version"] == 1,
            "run schema_version must be 1")
    require(run.get("kind") in {"rust", "gui"}, "run kind must be rust or gui")
    for field in ("mode", "dataset_manifest", "source", "host", "environment_id", "binary", "samples",
                  "process_observations", "boundaries"):
        require(field in run, "run missing field: " + field)
    nonempty(run["mode"], "mode")
    dataset = run["dataset_manifest"]
    require(isinstance(dataset, dict), "dataset_manifest must be an object")
    for field in ("path", "sha256", "dataset_sha256", "protocol_sha256", "fixed_workspace", "generation", "metadata"):
        require(field in dataset, "dataset_manifest missing field: " + field)
    dataset_path, _ = bound_file(run_path, dataset, inputs)
    _, dataset_document = document(dataset_path, inputs)
    require(not schema_example(dataset_document), "schema_only manifest is not measurement data")
    for field in ("dataset_sha256", "protocol_sha256", "fixed_workspace", "generation", "metadata", "comparison"):
        require(field in dataset_document and dataset.get(field) == dataset_document[field],
                "embedded dataset descriptor differs from bound manifest: " + field)
    require(isinstance(dataset_document.get("files"), list), "manifest files identity missing")
    for record in dataset_document["files"]:
        require(isinstance(record, dict) and isinstance(record.get("path"), str) and
                type(record.get("bytes")) is int and record["bytes"] >= 0,
                "invalid frozen manifest file record")
        digest(record.get("sha256"))
    require(digest(dataset["dataset_sha256"]) == canonical_sha256(dataset_document["files"]),
            "dataset identity differs from manifest file inventory")
    require("protocol" in dataset_document and digest(dataset["protocol_sha256"]) == canonical_sha256(dataset_document["protocol"]),
            "protocol identity differs from frozen protocol")
    source = run["source"]
    require(isinstance(source, dict), "source must be an object")
    for field in ("metadata_path", "metadata_sha256", "source_sha256", "capsule"):
        require(field in source, "source missing field: " + field)
    source_path, _ = bound_file(run_path, {"path": source["metadata_path"], "sha256": source["metadata_sha256"]}, inputs)
    _, metadata = document(source_path, inputs)
    require(metadata == source["capsule"], "source capsule differs from bound metadata")
    require(digest(metadata.get("source_sha256")) == digest(source["source_sha256"]),
            "source identity differs from bound metadata")
    for field in ("framework", "eko", "profiles", "path_dependencies"):
        require(field in metadata, "source capsule missing field: " + field)
    require(digest(source["source_sha256"]) == canonical_sha256({
        "framework": metadata["framework"]["sha256"], "eko": metadata["eko"]["sha256"],
        "profiles": metadata["profiles"]}), "source digest does not match capsule identities/profile")
    require(digest(run["environment_id"]) == canonical_sha256(run["host"]), "environment id differs from host")
    if run["kind"] == "rust":
        binary_path, binary_sha = bound_file(run_path, run["binary"], inputs)
        require(run["binary"].get("retained_profile") == metadata["profiles"], "binary profile differs from capsule")
        require(digest(metadata["profiles"]["retained_binaries"][str(binary_path)]) == binary_sha,
                "binary is not the qualified retained binary")
    else:
        for field in ("node", "vitest_entry"):
            bound_file(run_path, run["binary"][field], inputs)
    compilation = compile_qualification(source_path, metadata["profiles"]["compile_receipt"],
                                        inputs, metadata["profiles"],
                                        {key: metadata[key]["root"] for key in ("framework", "eko")},
                                        binary_path if run["kind"] == "rust" else None,
                                        binary_sha if run["kind"] == "rust" else None,
                                        {key: metadata[key] for key in ("framework", "eko")})
    samples_path, samples_sha = bound_file(run_path, run["samples"], inputs)
    rows = []
    indices = set()
    counts = {}
    with samples_path.open(encoding="utf-8") as stream:
        for number, line in enumerate(stream, 1):
            require(bool(line.strip()), "empty sample line: " + str(number))
            row = decode_json(line)
            allowed_fields = SAMPLE_FIELDS | (GUI_OBSERVATION_FIELDS if run["kind"] == "gui" else set())
            require(isinstance(row, dict) and SAMPLE_FIELDS <= set(row) <= allowed_fields,
                    "sample fields differ from exact contract at line " + str(number))
            require(not schema_example(row), "schema_only / EXAMPLE_ONLY sample is not measurement data")
            for field in ("scenario", "mode", "phase", "environment_id", "process_observation_id"):
                nonempty(row[field], field)
            require(row["environment_id"] == run["environment_id"], "sample environment differs from run")
            require(row["phase"] in ("warmup", "measurement"), "unknown sample phase")
            require(row["outcome"] in OUTCOMES, "unknown sample outcome")
            require(type(row["sample_index"]) is int and row["sample_index"] >= 0, "invalid sample_index")
            require(isinstance(row["elapsed_ns"], str) and re.fullmatch(r"[0-9]+", row["elapsed_ns"]) is not None,
                    "elapsed_ns must be a decimal integer string")
            require(int(row["elapsed_ns"]) <= MAX_SAFE_INTEGER and row["sample_index"] <= MAX_SAFE_INTEGER,
                    "sample exceeds Node summary safe numeric range")
            for field, expected in (("dataset_sha256", dataset["dataset_sha256"]),
                                    ("protocol_sha256", dataset["protocol_sha256"]),
                                    ("source_sha256", source["source_sha256"])):
                require(digest(row[field]) == digest(expected), "sample identity mismatch: " + field)
            require(correctness_ok(row["correctness"]), "sample correctness failed")
            unique = sample_key(row) + (row["phase"], row["sample_index"])
            require(unique not in indices, "duplicate sample index within group")
            indices.add(unique)
            group_count = counts.setdefault(sample_key(row), [0, 0])
            group_count[0 if row["phase"] == "measurement" else 1] += 1
            rows.append(row)
    require(type(run["samples"].get("rows")) is int and run["samples"]["rows"] == len(rows),
            "samples row count mismatch")
    require(bool(rows), "no actual samples")
    summary_path, summary = document(summary_path, inputs)
    require(type(summary.get("schema_version")) is int and summary["schema_version"] == 1,
            "summary schema_version must be 1")
    summary_samples_path, summary_samples_sha = bound_file(summary_path, summary.get("samples"), inputs)
    require(summary_samples_path == samples_path and summary_samples_sha == samples_sha,
            "summary is bound to different samples")
    require(summary["samples"].get("rows") == len(rows), "summary row count mismatch")
    groups = summary_groups(summary, counts)
    observations = load_observations(run_path, run["process_observations"], rows, inputs)
    identity, missing = comparison_identity(run, dataset_document, dataset_path, inputs)
    gui_identity, gui_gaps = gui_scope(rows, run["kind"])
    gaps = ["complete_required_matrix_not_independently_established",
            "real_RTL_interaction_not_established", "native_paint_not_established"]
    if run["kind"] == "gui":
        gaps.append("GUI_jsdom_or_other_synthetic_render_is_not_native_paint")
    gaps.extend(gui_gaps)
    if compilation["gap"]:
        gaps.append(compilation["gap"])
    gaps.extend("comparison_identity_missing:" + field for field in missing)
    failures = []
    for observation in observations:
        raw = observation["observation"]
        if raw["process"]["exit"] != 0:
            gaps.append("failed_or_aborted_process:" + str(raw["process"]["exit"]))
            failures.append({"process_observation_id": raw["process_observation_id"], "exit": raw["process"]["exit"],
                             "observation_path": observation["path"]})
    if failures:
        for group in groups:
            group["statistics_from_supplied_node_summary"] = None
            group["p99_status"] = "not_reported_failed_process"
    if run["kind"] == "gui" and len(gui_identity["identities"]) != 1:
        for group in groups:
            group["statistics_from_supplied_node_summary"] = None
            group["p99_status"] = "not_reported_mixed_GUI_scope"
    return {"run_path": str(run_path), "run_sha256": sha256_file(run_path),
            "summary_path": str(summary_path), "summary_sha256": sha256_file(summary_path),
            "kind": run["kind"], "mode": run["mode"], "source": source,
            "dataset_manifest": dataset, "host": run["host"], "binary": run["binary"],
            "samples": {"path": str(samples_path), "sha256": samples_sha, "rows": len(rows)},
            "groups": groups, "process_observations": observations,
            "comparison_identity": identity, "comparison_identity_missing": missing,
            "compile_qualification": compilation, "gui_scope_identity": gui_identity,
            "gui_observations": [{key: row[key] for key in ("scenario", "phase", "sample_index", *GUI_OBSERVATION_FIELDS)
                                  if key in row} for row in rows] if run["kind"] == "gui" else [],
            "environment_ids": sorted({row["environment_id"] for row in rows}),
            "boundaries": run["boundaries"], "coverage_status": "invalid" if failures else "incomplete",
            "failed_or_aborted_processes": failures, "gaps": gaps}


def compare_runs(before, after):
    reasons = []
    for field in ("kind", "mode", "comparison_identity"):
        if before[field] != after[field]:
            reasons.append("identity_mismatch:" + field)
    if before["comparison_identity_missing"] or after["comparison_identity_missing"]:
        reasons.append("unproven_comparison_identity")
    if any(run["compile_qualification"]["status"] != "recognized_success" for run in (before, after)):
        reasons.append("unproven_compile_qualification")
    if before["kind"] == "gui" or after["kind"] == "gui":
        if before["gui_scope_identity"] != after["gui_scope_identity"]:
            reasons.append("GUI_scope_identity_mismatch")
        if before["gui_scope_identity"]["missing"] or after["gui_scope_identity"]["missing"]:
            reasons.append("GUI_scope_identity_incomplete")
        if any(len(run["gui_scope_identity"]["identities"]) != 1 for run in (before, after)):
            reasons.append("GUI_scope_changes_within_distribution")
    for field in ("dataset_sha256", "protocol_sha256"):
        if digest(before["dataset_manifest"][field]) != digest(after["dataset_manifest"][field]):
            reasons.append("identity_mismatch:" + field)
    if before["failed_or_aborted_processes"] or after["failed_or_aborted_processes"]:
        reasons.append("failed_or_aborted_process")
    def comparison_groups(run):
        return {json.dumps({key: value for key, value in group["identity"].items()
                            if key not in ("source_sha256", "environment_id")}, sort_keys=True): group
                for group in run["groups"]}
    left, right = comparison_groups(before), comparison_groups(after)
    if set(left) != set(right):
        reasons.append("scenario_mode_outcome_groups_differ")
    return {"status": "not_comparable" if reasons else "descriptive_only",
            "reasons": reasons,
            "source_before": before["source"]["source_sha256"],
            "source_after": after["source"]["source_sha256"],
            "source_changed": digest(before["source"]["source_sha256"]) != digest(after["source"]["source_sha256"]),
            "environment_before": before["environment_ids"], "environment_after": after["environment_ids"],
            "environment_changed": before["environment_ids"] != after["environment_ids"],
            "groups": [] if reasons else [{"comparison_identity": json.loads(key),
                "before": left[key]["statistics_from_supplied_node_summary"],
                "after": right[key]["statistics_from_supplied_node_summary"]} for key in sorted(left)],
            "limits": "No SLA, causation, optimization conclusion, or new percentile calculation."}


def markdown(report):
    lines = ["# Offline performance evidence", "", "Status: " + report["status"], "",
             "This report does not establish an SLA, causation, or optimization result.", "",
             "Summary input bytes, group identities and counts are checked. Supplied Node statistics are not independently recomputed.", ""]
    for error in report.get("errors", []):
        lines.append("- Invalid: " + str(error).replace("\n", " "))
    for run in report.get("runs", []):
        lines.extend(["", "## " + run["kind"] + " / " + run["mode"], "",
                      "Samples SHA256: `" + run["samples"]["sha256"] + "`", "",
                      "| scenario | mode | phase | outcome | N | p99 |",
                      "| --- | --- | --- | --- | ---: | --- |"])
        for group in run["groups"]:
            fields = [str(group["identity"][key]).replace("|", "\\|").replace("\n", " ")
                      for key in ("scenario", "mode")]
            fields.extend([group["phase"], group["identity"]["outcome"]])
            lines.append("| " + " | ".join(fields + [str(group["sample_count"]), group["p99_status"]]) + " |")
        lines.extend(["", "Process CPU/RSS remain process-scoped; block counts are not physical bytes.", ""])
        lines.extend(["Supplied Node summaries (measurement only; no recomputation):", "", "```json",
                      json.dumps([group["statistics_from_supplied_node_summary"] for group in run["groups"]], indent=2), "```", ""])
        for item in run["process_observations"]:
            process = item["observation"]["process"]
            lines.append("- Process observation: " + json.dumps({
                "id": item["observation"]["process_observation_id"], "scope": process["scope"],
                "exit": process["exit"], "user_cpu_seconds": process["user_cpu_seconds"],
                "system_cpu_seconds": process["system_cpu_seconds"], "peak_rss_bytes": process["peak_rss_bytes"],
                "input_block_operations": process["input_block_operations"],
                "output_block_operations": process["output_block_operations"]}))
        lines.extend("- Gap: " + gap for gap in run["gaps"])
    if report.get("comparison"):
        lines.extend(["", "## Comparison", "", "```json", json.dumps(report["comparison"], indent=2), "```"])
    return "\n".join(lines) + "\n"


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--run", required=True, type=Path)
    parser.add_argument("--summary", required=True, type=Path)
    parser.add_argument("--compare-run", type=Path)
    parser.add_argument("--compare-summary", type=Path)
    parser.add_argument("--output-json", type=Path)
    parser.add_argument("--output-markdown", type=Path)
    args = parser.parse_args(argv)
    inputs = {p.resolve() for p in (args.run, args.summary, args.compare_run, args.compare_summary) if p}
    report = {"schema_version": 1, "kind": "offline_performance_coverage_report",
              "evidence_reader": {"path": str(Path(__file__).resolve()), "sha256": sha256_file(Path(__file__))},
              "status": "invalid", "runs": [], "errors": [], "comparison": None,
              "validation_scope": "Offline byte/hash, identity, oracle, group-count and process-observation checks; explicit snapshot inputs also rescan their bound material trees. No dataset regeneration or independent percentile calculation.",
              "completeness_policy": "Missing required-matrix, real RTL and native-paint proof remains incomplete; no SLA or causal/optimization verdict."}
    try:
        require(bool(args.compare_run) == bool(args.compare_summary), "comparison requires both run and summary")
        report["runs"].append(load_run(args.run, args.summary, inputs))
        if args.compare_run:
            report["runs"].append(load_run(args.compare_run, args.compare_summary, inputs))
            report["comparison"] = compare_runs(*report["runs"])
        failures = [failure for run in report["runs"] for failure in run["failed_or_aborted_processes"]]
        report["status"] = "invalid" if failures else "incomplete"
        if failures:
            report["errors"].append({"failed_or_aborted_processes": failures})
    except (InvalidEvidence, OSError, ValueError, KeyError, TypeError, OverflowError) as error:
        report["errors"].append(str(error))
        # An aborted.json is not a run envelope, but retain its actual failure
        # markers and byte identity rather than presenting an empty green report.
        try:
            _, rejected = document(args.run, inputs)
            report["rejected_run"] = {"path": str(args.run.resolve()), "sha256": sha256_file(args.run),
                "failure_fields": {key: rejected[key] for key in
                    ("exit", "reason", "error", "accepted_samples", "status", "aborted") if key in rejected}}
        except (InvalidEvidence, OSError, ValueError, KeyError, TypeError, OverflowError):
            pass
    rendered = json.dumps(report, ensure_ascii=False, indent=2, allow_nan=False) + "\n"
    destinations = [p.resolve() for p in (args.output_json, args.output_markdown) if p]
    try:
        require(len(destinations) == len(set(destinations)), "output paths must be distinct")
        for destination in destinations:
            require(destination not in inputs and not destination.exists(), "output would overwrite evidence/existing file")
        for destination, content in ((args.output_json, rendered), (args.output_markdown, markdown(report))):
            if destination:
                with destination.open("x", encoding="utf-8") as stream:
                    stream.write(content)
    except (InvalidEvidence, OSError) as error:
        print("report output refused: " + str(error), file=sys.stderr)
        return 1
    if not args.output_json:
        print(rendered, end="")
    return 1 if report["status"] == "invalid" else 2


if __name__ == "__main__":
    sys.exit(main())
