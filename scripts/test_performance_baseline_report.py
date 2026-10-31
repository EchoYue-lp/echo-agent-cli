#!/usr/bin/env python3
"""Tiny synthetic schema/negative tests; none are performance measurements."""
import contextlib
import importlib.util
import io
import json
from pathlib import Path
import tempfile
import unittest
from test_performance_baseline_runner import snapshot_fixture, RUNNER


SCRIPT = Path(__file__).with_name("performance-baseline-report.py")
SPEC = importlib.util.spec_from_file_location("performance_report_candidate", SCRIPT)
REPORT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(REPORT)


class SyntheticEvidenceTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name).resolve()
        self.addCleanup(self.temporary.cleanup)

    def write(self, path, value):
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(value) + "\n", encoding="utf-8")
        return path

    def descriptor(self, path):
        return {"path": str(path), "sha256": REPORT.sha256_file(path)}

    def fixture(self, name="one", outcome="success", source="a" * 64):
        root = self.root / name
        root.mkdir()
        files = [{"path": "fixture.json", "bytes": 2, "sha256": "d" * 64}]
        protocol = {"scope": "synthetic-only-not-real-protocol"}
        dataset_digest, protocol_digest = REPORT.canonical_sha256(files), REPORT.canonical_sha256(protocol)
        units = {"conversations": "count", "messages": "count"}
        profiles = {"features": {
            "framework": {"default_features": True, "all_features": False, "explicit": ["mcp"]},
            "eko": {"default_features": True, "all_features": False, "explicit": []}},
            "profile": "release", "durability": "file"}
        binary = root / "synthetic-binary.txt"
        binary.write_text("not executable; never run\n")
        # Shape matches an original Supreme run-command receipt. These bytes
        # remain a tiny synthetic control; no Cargo process is started.
        compile_receipt = self.write(root / "compile.json", {
            "label": "synthetic-compile", "command": ["env", "CARGO_BUILD_JOBS=1", "CARGO_PROFILE_RELEASE_DEBUG=0", "cargo", "test", "-p", "echo_agent", "--release", "--features", "mcp", "--no-run"],
            "cwd": str(root), "logPath": str(root / "compile.log"), "exitCode": 0,
            "signal": None, "errorCode": None, "truncated": False, "bytesSeen": 0, "bytesWritten": 0,
            "startedAt": "2026-10-24T00:00:00.000Z", "finishedAt": "2026-10-24T00:00:01.000Z"})
        profiles.update(compile_receipt=self.descriptor(compile_receipt),
                        retained_binaries={str(binary): REPORT.sha256_file(binary)})
        actual = json.loads(compile_receipt.read_text())
        profiles["compile_receipt"]["final_steps"] = [{"receipt": self.descriptor(compile_receipt),
            "label": actual["label"], "command": actual["command"], "cwd": str(root), "repository": "framework",
            "profile": "release", "features": profiles["features"], "retained_binaries": profiles["retained_binaries"]}]
        capsule = {"framework": {"sha256": source, "root": str(root)}, "eko": {"sha256": "f" * 64, "root": str(root)},
                   "profiles": profiles, "path_dependencies": []}
        source = REPORT.canonical_sha256({"framework": capsule["framework"]["sha256"],
                                         "eko": capsule["eko"]["sha256"], "profiles": profiles})
        capsule["source_sha256"] = source
        metadata = self.write(root / "nested" / "source.json", capsule)
        comparison = {key: profiles[key] for key in ("features", "profile", "durability")}
        comparison.update(units=units, toolset_sha256=None)
        manifest_value = {"dataset_sha256": dataset_digest, "protocol_sha256": protocol_digest,
                          "files": files, "protocol": protocol, "fixed_workspace": "synthetic-only",
                          "generation": {"case": "synthetic", "units": units},
                          "metadata": {}, "comparison": comparison}
        manifest = self.write(root / "manifest.json", manifest_value)
        stdout = root / "stdout.log"
        stderr = root / "stderr.log"
        stdout.write_text("synthetic-only\n")
        stderr.write_text("")
        process = {"schema_version": 1, "process_observation_id": "proc-synthetic",
                   "process": {"args": ["synthetic"], "exit": 0,
                      "wall_including_launch_setup_and_validation_ns": "9",
                      "user_cpu_seconds": 0.1, "system_cpu_seconds": 0.01,
                      "peak_rss_bytes": 64, "scope": "synthetic-child-process",
                      "input_block_operations": 1, "output_block_operations": 2,
                      "block_io_limit": "operation counts, not physical bytes",
                      "load_before": [0.1], "load_after": [0.1],
                      "stdout": self.descriptor(stdout), "stderr": self.descriptor(stderr)},
                   "input_copy_and_verification_ns_excluded_from_child": "1",
                   "operation_row_count": 1, "rss_cpu_io_not_per_operation": True}
        observation = self.write(root / "observation.json", process)
        host = {"synthetic": True}
        environment_id = REPORT.canonical_sha256(host)
        row = {"scenario": "conversation-list", "mode": "synthetic", "phase": "measurement",
               "sample_index": 0, "elapsed_ns": "123", "outcome": outcome,
               "dataset_sha256": dataset_digest, "source_sha256": source,
               "protocol_sha256": protocol_digest, "environment_id": environment_id,
               "process_observation_id": "proc-synthetic", "correctness": {"ok": True, "scope": "tiny synthetic unit test"}}
        samples = self.write(root / "samples.jsonl", row)
        run = {"schema_version": 1, "kind": "rust", "mode": "synthetic",
               "dataset_manifest": dict(self.descriptor(manifest), dataset_sha256=dataset_digest,
                   protocol_sha256=protocol_digest, fixed_workspace="synthetic-only",
                   generation=manifest_value["generation"], metadata={}, comparison=comparison),
               "source": {"metadata_path": str(metadata), "metadata_sha256": REPORT.sha256_file(metadata),
                          "source_sha256": source, "capsule": capsule},
               "host": host, "environment_id": environment_id,
               "binary": dict(self.descriptor(binary), retained_profile=profiles),
               "samples": dict(self.descriptor(samples), rows=1),
               "process_observations": [self.descriptor(observation)],
               "boundaries": ["tiny synthetic schema test; never actual benchmark"]}
        run_path = self.write(root / "run.json", run)
        summary = {"schema_version": 1, "samples": dict(self.descriptor(samples), rows=1),
                   "groups": [{**{key: row[key] for key in REPORT.GROUP_FIELDS},
                               "unit": "ns", "n": 1, "warmup_n": 0,
                               "min": 123, "median": 123, "max": 123}]}
        summary_path = self.write(root / "summary.json", summary)
        return run_path, summary_path

    def load(self, pair):
        return REPORT.load_run(*pair)

    def test_valid_synthetic_evidence_is_incomplete_and_process_scoped(self):
        run = self.load(self.fixture())
        self.assertEqual(run["coverage_status"], "incomplete")
        self.assertFalse(run["comparison_identity_missing"])
        process = run["process_observations"][0]
        self.assertIn("process_only", process["scope"])
        self.assertEqual(process["observation"]["process"]["input_block_operations"], 1)
        self.assertNotIn("p99_ns", run["groups"][0]["statistics_from_supplied_node_summary"])

    def test_sample_bytes_drift_is_rejected(self):
        pair = self.fixture()
        pair[0].with_name("samples.jsonl").write_text("{}\n")
        with self.assertRaisesRegex(REPORT.InvalidEvidence, "byte hash mismatch"):
            self.load(pair)

    def test_summary_bound_to_different_samples_is_rejected(self):
        pair, other = self.fixture(), self.fixture("other")
        summary = json.loads(pair[1].read_text())
        summary["samples"] = dict(self.descriptor(other[0].with_name("samples.jsonl")), rows=1)
        self.write(pair[1], summary)
        with self.assertRaisesRegex(REPORT.InvalidEvidence, "different samples"):
            self.load(pair)

    def rewrite_row(self, pair, update):
        sample = pair[0].with_name("samples.jsonl")
        row = json.loads(sample.read_text())
        update(row)
        self.write(sample, row)
        for path in pair:
            doc = json.loads(path.read_text())
            doc["samples"] = dict(self.descriptor(sample), rows=1)
            self.write(path, doc)

    def test_failed_oracle_is_rejected_even_with_rebound_hashes(self):
        pair = self.fixture()
        self.rewrite_row(pair, lambda row: row.update(correctness={"ok": False}))
        with self.assertRaisesRegex(REPORT.InvalidEvidence, "correctness failed"):
            self.load(pair)

    def test_missing_sample_field_is_rejected(self):
        pair = self.fixture()
        self.rewrite_row(pair, lambda row: row.pop("elapsed_ns"))
        with self.assertRaisesRegex(REPORT.InvalidEvidence, "sample fields"):
            self.load(pair)

    def test_real_gui_observation_shape_keeps_common_required_identity(self):
        pair = self.fixture()
        run = json.loads(pair[0].read_text())
        run["kind"] = "gui"
        run["binary"] = {"node": run["binary"], "vitest_entry": run["binary"]}
        self.write(pair[0], run)
        extras = {"profiler_commits": [{"phase": "mount", "actualDurationMs": 0.01, "baseDurationMs": 0.02}],
                  "dom_elements": 10, "clock": "performance.now; milliseconds converted to ns, not ns precision",
                  "viewport": {"width": 1400, "height": 900}, "theme": "dark", "subagent_history": 0,
                  "native_layout_paint": "not_measured", "cpu_rss_io": "separate whole-process observation; not per render",
                  "mock_boundary": "typed stores receive accepted fixture; no backend/LLM/network/polling"}
        self.rewrite_row(pair, lambda row: row.update(extras))
        observed = self.load(pair)
        self.assertEqual(observed["kind"], "gui")
        self.assertEqual(observed["coverage_status"], "incomplete")
        self.rewrite_row(pair, lambda row: row.pop("source_sha256"))
        with self.assertRaisesRegex(REPORT.InvalidEvidence, "sample fields"):
            self.load(pair)

    def test_gui_extras_do_not_silently_expand_rust_row_contract(self):
        pair = self.fixture()
        self.rewrite_row(pair, lambda row: row.update(dom_elements=10))
        with self.assertRaisesRegex(REPORT.InvalidEvidence, "sample fields"):
            self.load(pair)

    def rewrite_compilation(self, pair, update, change_final=None):
        run = json.loads(pair[0].read_text())
        capsule = run["source"]["capsule"]
        compile_path = Path(capsule["profiles"]["compile_receipt"]["path"])
        receipt = json.loads(compile_path.read_text())
        self.write(compile_path, update(receipt))
        finals = capsule["profiles"]["compile_receipt"]["final_steps"]
        for final in finals:
            final["receipt"] = self.descriptor(compile_path)
        if change_final:
            change_final(finals)
        capsule["profiles"]["compile_receipt"] = {**self.descriptor(compile_path), "final_steps": finals}
        identity = REPORT.canonical_sha256({"framework": capsule["framework"]["sha256"],
                                           "eko": capsule["eko"]["sha256"], "profiles": capsule["profiles"]})
        capsule["source_sha256"] = identity
        source_path = Path(run["source"]["metadata_path"])
        self.write(source_path, capsule)
        run["source"].update(metadata_sha256=REPORT.sha256_file(source_path), source_sha256=identity)
        if run["kind"] == "rust":
            run["binary"]["retained_profile"] = capsule["profiles"]
        self.write(pair[0], run)
        self.rewrite_row(pair, lambda row: row.update(source_sha256=identity))
        summary = json.loads(pair[1].read_text())
        summary["groups"][0]["source_sha256"] = identity
        self.write(pair[1], summary)

    def test_compile_failure_signal_and_truncation_are_invalid_not_fast_success(self):
        for key, value in (("exitCode", 1), ("exitCode", None), ("signal", "SIGTERM"),
                           ("errorCode", "ENOENT"), ("truncated", True)):
            with self.subTest(key=key, value=value):
                pair = self.fixture("failed-" + key + str(value))
                self.rewrite_compilation(pair, lambda receipt: {**receipt, key: value})
                with contextlib.redirect_stdout(io.StringIO()) as output:
                    code = REPORT.main(["--run", str(pair[0]), "--summary", str(pair[1])])
                report = json.loads(output.getvalue())
                self.assertEqual(code, 1)
                self.assertEqual(report["status"], "invalid")
                self.assertFalse(report["runs"])
                self.assertIsNone(report["comparison"])

    def test_original_supreme_run_store_qualifies_actual_successful_compile_step(self):
        pair = self.fixture()
        self.rewrite_compilation(pair, lambda receipt: {"schemaVersion": 1, "sourceRoot": receipt["cwd"], "steps": [receipt]})
        loaded = self.load(pair)
        self.assertEqual(loaded["compile_qualification"]["status"], "recognized_success")

    def test_unknown_compile_receipt_is_a_gap_and_not_comparable(self):
        pair = self.fixture()
        self.rewrite_compilation(pair, lambda _receipt: {"synthetic": True})
        loaded = self.load(pair)
        self.assertIn("compile_receipt_schema_not_recognized", loaded["gaps"])
        self.assertEqual(REPORT.compare_runs(loaded, loaded)["status"], "not_comparable")

    def test_failed_history_does_not_replace_explicit_final_success(self):
        pair = self.fixture()
        self.rewrite_compilation(pair, lambda receipt: {"schemaVersion": 1, "sourceRoot": receipt["cwd"],
            "steps": [{**receipt, "label": "previous-failed", "exitCode": 1}, receipt]})
        loaded = self.load(pair)
        self.assertEqual(loaded["compile_qualification"]["status"], "recognized_success")
        self.assertEqual(loaded["compile_qualification"]["steps"][0]["other_original_steps"][0]["exitCode"], 1)

    def test_unrelated_success_does_not_replace_missing_final_step(self):
        pair = self.fixture()
        self.rewrite_compilation(pair, lambda receipt: {**receipt, "label": "unrelated-success"})
        loaded = self.load(pair)
        self.assertIn("compile_final_step_not_found", loaded["gaps"])
        self.assertEqual(REPORT.compare_runs(loaded, loaded)["status"], "not_comparable")

    def test_duplicate_final_label_or_binary_descriptor_is_ambiguous(self):
        for duplicate in ("step", "descriptor"):
            pair = self.fixture(duplicate)
            self.rewrite_compilation(pair,
                lambda receipt: {"schemaVersion": 1, "steps": [receipt, receipt]} if duplicate == "step" else receipt,
                (lambda finals: finals.append(dict(finals[0]))) if duplicate == "descriptor" else None)
            with self.assertRaisesRegex(REPORT.InvalidEvidence, "ambiguous"):
                self.load(pair)

    def test_final_command_and_feature_identity_cannot_be_rebound_to_another_success(self):
        for field, value in (("command", ["cargo", "test", "--no-run"]), ("features", ["unrelated-feature"]),
                             ("cwd", "/unrelated-cwd")):
            pair = self.fixture(field)
            self.rewrite_compilation(pair, lambda receipt: receipt,
                                     lambda finals: finals[0].update({field: value}))
            with self.assertRaises(REPORT.InvalidEvidence):
                self.load(pair)

    def as_gui(self, pair, **scope_changes):
        run = json.loads(pair[0].read_text())
        binary = {"path": run["binary"]["path"], "sha256": run["binary"]["sha256"]}
        run.update(kind="gui", binary={"node": binary, "vitest_entry": binary})
        self.write(pair[0], run)
        eko_command = ["env", "CARGO_BUILD_JOBS=1", "cargo", "test", "-p", "echo-agent-app-core", "--release", "--no-run"]
        self.rewrite_compilation(pair, lambda receipt: {"schemaVersion": 1, "steps": [receipt,
            {**receipt, "label": "synthetic-eko-compile", "command": eko_command}]},
            lambda finals: finals.append({**finals[0], "label": "synthetic-eko-compile",
                                         "command": eko_command, "repository": "eko"}))
        scope = {"theme": "dark", "viewport": {"width": 1400, "height": 900},
                 "native_layout_paint": "not_measured", "mock_boundary": "typed stores; backend absent",
                 "clock": "performance.now", "subagent_history": 0, "cpu_rss_io": "whole process"}
        scope.update(scope_changes)
        self.rewrite_row(pair, lambda row: row.update(scope))
        return self.load(pair)

    def test_gui_input_scope_is_preserved_and_different_inputs_cannot_compare(self):
        before = self.as_gui(self.fixture())
        self.assertEqual(before["gui_observations"][0]["theme"], "dark")
        self.assertEqual(before["compile_qualification"]["status"], "recognized_success")
        for index, (key, value) in enumerate((("theme", "light"), ("viewport", {"width": 800, "height": 600}),
                                            ("mock_boundary", "another backend"), ("native_layout_paint", "claimed-native"),
                                            ("subagent_history", 1), ("clock", "another clock"))):
            after = self.as_gui(self.fixture("gui-" + str(index)), **{key: value})
            self.assertEqual(REPORT.compare_runs(before, after)["status"], "not_comparable")
            self.assertIn("native_paint_not_established", after["gaps"])

    def test_metadata_feature_agreement_cannot_override_real_argv(self):
        pair = self.fixture()
        def change(finals):
            finals[0]["features"]["framework"]["explicit"] = ["unrelated"]
        # Updating both metadata copies is still inconsistent with --features mcp.
        run = json.loads(pair[0].read_text())
        run["source"]["capsule"]["profiles"]["features"]["framework"]["explicit"] = ["unrelated"]
        self.write(pair[0], run)
        self.rewrite_compilation(pair, lambda receipt: receipt, change)
        with self.assertRaisesRegex(REPORT.InvalidEvidence, "actual repository Cargo argv"):
            self.load(pair)

    def test_actual_cargo_selection_handles_feature_flags_and_profile_semantics(self):
        for args, expected in [
            (["cargo", "test", "--no-run"], {"profile": "test", "features": {"default_features": True, "all_features": False, "explicit": []}}),
            (["cargo", "test", "--profile", "custom", "--features", "files,mcp", "-F", "mcp shell", "-Ffiles", "--no-default-features"],
             {"profile": "custom", "features": {"default_features": False, "all_features": False, "explicit": ["files", "mcp", "shell"]}}),
            (["cargo", "test", "--profile=release", "--all-features"],
             {"profile": "release", "features": {"default_features": True, "all_features": True, "explicit": []}})]:
            self.assertEqual(REPORT.cargo_selection(args), (expected, None))
        for args in (["cargo", "test", "--release", "--profile", "custom"],
                     ["cargo", "test", "--all-features", "--no-default-features"],
                     ["cargo", "test", "--all-features", "-Ffiles"], ["cargo", "test", "-Z", "unstable"]):
            self.assertIsNotNone(REPORT.cargo_selection(args)[1])

    def as_native_gui_compile(self, pair, command=None):
        run = json.loads(pair[0].read_text())
        features = run["source"]["capsule"]["profiles"]["features"]
        features["eko"] = {"default_features": False, "all_features": False, "explicit": ["gui"]}
        run["source"]["capsule"]["profiles"]["features"] = features
        manifest_path = Path(run["dataset_manifest"]["path"])
        manifest = json.loads(manifest_path.read_text())
        manifest["comparison"]["features"] = features
        self.write(manifest_path, manifest)
        run["dataset_manifest"].update(self.descriptor(manifest_path), comparison=manifest["comparison"])
        self.write(pair[0], run)
        if command is None:
            command = ["env", "CARGO_BUILD_JOBS=5", "CARGO_INCREMENTAL=0",
                       "TMPDIR=" + str(self.root / "isolated-tmp"), "cargo", "test", "-p", "echo-agent-cli",
                       "--release", "--no-default-features", "--features", "gui", "--lib", "--locked", "--no-run"]

        def change(finals):
            for final in finals:
                final.update(repository="eko", command=command, features=features)

        self.rewrite_compilation(pair, lambda receipt: {**receipt, "command": command}, change)

    def test_native_gui_lib_exact_compile_and_controlled_env_are_qualified(self):
        pair = self.fixture()
        self.as_native_gui_compile(pair)
        value = self.load(pair)
        self.assertEqual(value["kind"], "rust")
        self.assertEqual(value["compile_qualification"]["status"], "recognized_success")
        self.assertEqual(value["coverage_status"], "incomplete")
        self.assertIn("native_paint_not_established", value["gaps"])

    def test_native_gui_compile_does_not_accept_other_packages_or_target_aliases(self):
        base = ["cargo", "test", "-p", "echo-agent-cli", "--release", "--no-default-features",
                "--features", "gui", "--lib", "--locked", "--no-run"]
        variants = [base[:3] + ["unrelated-package"] + base[4:],
                    base + ["-p", "echo-agent-app-core"],
                    [arg for arg in base if arg != "--lib"],
                    [arg for arg in base if arg != "--locked"],
                    base + ["--test", "performance_baseline"],
                    base[:3] + ["echo-agent-cli@1.0.0"] + base[4:]]
        for index, command in enumerate(variants):
            pair = self.fixture("bad-native-target-" + str(index))
            self.as_native_gui_compile(pair, command)
            with self.assertRaises(REPORT.InvalidEvidence):
                self.load(pair)

    def test_native_gui_and_default_appcore_features_cannot_share_a_capsule(self):
        pair = self.fixture()
        self.as_native_gui_compile(pair, ["cargo", "test", "-p", "echo-agent-app-core", "--release", "--no-run"])
        with self.assertRaisesRegex(REPORT.InvalidEvidence, "actual repository Cargo argv"):
            self.load(pair)

    def test_incremental_and_tmpdir_prefix_do_not_expand_arbitrary_env(self):
        for assignment in ("CARGO_INCREMENTAL=1", "TMPDIR=relative", "RUSTFLAGS=unbound"):
            self.assertIsNone(REPORT.cargo_arguments(["env", assignment, "cargo", "test", "--no-run"]))
        pair = self.fixture()
        self.as_native_gui_compile(pair, ["env", "CARGO_INCREMENTAL=0", "TMPDIR=/outside-qualified-workspace",
            "cargo", "test", "-p", "echo-agent-cli", "--release", "--no-default-features", "--features", "gui",
            "--lib", "--locked", "--no-run"])
        with self.assertRaisesRegex(REPORT.InvalidEvidence, "TMPDIR.*outside"):
            self.load(pair)

    def test_native_scope_and_attempt_success_counts_stay_nested_in_correctness(self):
        pair = self.fixture()
        observation = {"ok": True, "scope": "ordinary native caller; renderer and network excluded",
                       "canonical_sha256": "c" * 64, "publication_sha256": "d" * 64,
                       "dto_sha256": "e" * 64,
                       "io": {"coverage_load": {"attempts": 2, "successes": 2}}}
        self.rewrite_row(pair, lambda row: row.update(correctness=observation))
        self.assertEqual(self.load(pair)["coverage_status"], "incomplete")
        self.assertEqual(json.loads(pair[0].with_name("samples.jsonl").read_text())["correctness"], observation)
        self.rewrite_row(pair, lambda row: row.update(native_scope="ordinary"))
        with self.assertRaisesRegex(REPORT.InvalidEvidence, "sample fields"):
            self.load(pair)

    def test_unbound_source_target_and_configuration_selectors_are_gaps(self):
        for option, value in (("--manifest-path", "/another-checkout/Cargo.toml"),
                              ("--manifest-path", "/same-checkout/Cargo.toml"),
                              ("--target", "another-target"), ("--target-dir", "/another-target-dir"),
                              ("--config", "source.crates-io.replace-with=other"),
                              ("--package", "echo_agent"), ("--exclude", "other"),
                              ("--test", "another-integration-target")):
            selection, gap = REPORT.cargo_selection(["cargo", "test", option, value, "--no-run"])
            self.assertIsNone(selection)
            self.assertIsNotNone(gap)
        self.assertIsNone(REPORT.cargo_arguments(["env", "CARGO_ENCODED_RUSTFLAGS=unbound", "cargo", "test", "--no-run"]))

    def test_gui_missing_other_repository_is_gap_and_its_failure_is_invalid(self):
        pair = self.fixture()
        run = json.loads(pair[0].read_text())
        binary = {"path": run["binary"]["path"], "sha256": run["binary"]["sha256"]}
        run.update(kind="gui", binary={"node": binary, "vitest_entry": binary})
        self.write(pair[0], run)
        value = self.load(pair)
        self.assertIn("GUI_compile_repository_coverage_incomplete", value["gaps"])
        self.assertEqual(REPORT.compare_runs(value, value)["status"], "not_comparable")
        other = self.fixture("other-repo-failure")
        self.as_gui(other)
        self.rewrite_compilation(other, lambda receipt: {**receipt, "steps": [
            {**step, "exitCode": 1} if step["label"] == "synthetic-eko-compile" else step
            for step in receipt["steps"]]})
        with contextlib.redirect_stdout(io.StringIO()) as output:
            code = REPORT.main(["--run", str(other[0]), "--summary", str(other[1])])
        self.assertEqual(code, 1)
        self.assertFalse(json.loads(output.getvalue())["runs"])

    def test_refusal_and_success_are_not_comparable(self):
        success = self.load(self.fixture())
        refusal = self.load(self.fixture("refusal", outcome="expected_refusal"))
        self.assertEqual(REPORT.compare_runs(success, refusal)["status"], "not_comparable")

    def test_new_source_is_explicit_but_not_itself_comparison_disqualifier(self):
        before = self.load(self.fixture())
        after = self.load(self.fixture("changed", source="b" * 64))
        result = REPORT.compare_runs(before, after)
        self.assertEqual(result["status"], "descriptive_only")
        self.assertTrue(result["source_changed"])

    def test_p99_boundary_uses_actual_sample_count_no_percentile_algorithm(self):
        raw = {"p50_ns": "1", "nested": {"p99_ns": "3"}}
        self.assertNotIn("p99_ns", REPORT.strip_unqualified_p99(raw, 99)["nested"])
        self.assertEqual(REPORT.strip_unqualified_p99(raw, 100), raw)

    def test_reference_resolves_against_own_document_not_cwd(self):
        parent = self.root / "nested" / "source.json"
        target = self.write(self.root / "nested" / "evidence.json", {})
        self.assertEqual(REPORT.referenced(parent, "evidence.json"), target)

    def test_cli_does_not_report_incomplete_evidence_as_green(self):
        pair = self.fixture()
        with contextlib.redirect_stdout(io.StringIO()) as output:
            code = REPORT.main(["--run", str(pair[0]), "--summary", str(pair[1])])
        self.assertEqual(code, 2)
        self.assertEqual(json.loads(output.getvalue())["status"], "incomplete")

    def test_output_cannot_overwrite_input(self):
        pair = self.fixture()
        original = pair[0].read_bytes()
        with contextlib.redirect_stderr(io.StringIO()):
            code = REPORT.main(["--run", str(pair[0]), "--summary", str(pair[1]),
                                "--output-json", str(pair[0])])
        self.assertEqual(code, 1)
        self.assertEqual(pair[0].read_bytes(), original)

    def test_duplicate_json_keys_are_rejected(self):
        with self.assertRaisesRegex(REPORT.InvalidEvidence, "duplicate JSON key"):
            REPORT.decode_json('{"rows":1,"rows":2}')

    def mutate_observation(self, pair, update):
        run = json.loads(pair[0].read_text())
        path = Path(run["process_observations"][0]["path"])
        value = json.loads(path.read_text())
        update(value)
        self.write(path, value)
        run["process_observations"][0] = self.descriptor(path)
        self.write(pair[0], run)

    def test_failed_child_is_explicit_invalid_and_not_comparable(self):
        pair = self.fixture()
        self.mutate_observation(pair, lambda value: value["process"].update(exit=-9))
        run = self.load(pair)
        self.assertEqual(run["coverage_status"], "invalid")
        self.assertEqual(run["failed_or_aborted_processes"][0]["exit"], -9)
        self.assertIsNone(run["groups"][0]["statistics_from_supplied_node_summary"])
        self.assertEqual(REPORT.compare_runs(run, run)["status"], "not_comparable")

    def test_observation_id_must_bind_sample_rows(self):
        pair = self.fixture()
        self.mutate_observation(pair, lambda value: value.update(process_observation_id="wrong-process"))
        with self.assertRaises(REPORT.InvalidEvidence):
            self.load(pair)

    def test_raw_stdout_drift_is_not_hidden(self):
        pair = self.fixture()
        pair[0].with_name("stdout.log").write_text("changed\n")
        with self.assertRaisesRegex(REPORT.InvalidEvidence, "byte hash mismatch"):
            self.load(pair)

    def test_comparison_projection_cannot_override_capsule(self):
        pair = self.fixture()
        run = json.loads(pair[0].read_text())
        run["dataset_manifest"]["comparison"]["profile"] = "unqualified-new-profile"
        self.write(pair[0], run)
        with self.assertRaisesRegex(REPORT.InvalidEvidence, "differs"):
            self.load(pair)

    def test_schema_only_and_example_identifiers_never_enter_statistics(self):
        pair = self.fixture()
        run = json.loads(pair[0].read_text())
        run["boundaries"] = {"schema_example_only": True}
        self.write(pair[0], run)
        with contextlib.redirect_stdout(io.StringIO()) as output:
            code = REPORT.main(["--run", str(pair[0]), "--summary", str(pair[1])])
        result = json.loads(output.getvalue())
        self.assertEqual(code, 1)
        self.assertFalse(result["runs"])
        self.assertIn("schema_only", result["errors"][0])

    def test_warmup_and_run_label_remain_distinct(self):
        pair = self.fixture()
        sample = pair[0].with_name("samples.jsonl")
        measured = json.loads(sample.read_text())
        warmup = dict(measured, phase="warmup")
        sample.write_text(json.dumps(warmup) + "\n" + json.dumps(measured) + "\n")
        run = json.loads(pair[0].read_text())
        run.update(mode="runtime-cold")
        run["samples"] = dict(self.descriptor(sample), rows=2)
        self.write(pair[0], run)
        self.mutate_observation(pair, lambda value: value.update(operation_row_count=2))
        summary = json.loads(pair[1].read_text())
        summary["samples"] = dict(self.descriptor(sample), rows=2)
        summary["groups"][0]["warmup_n"] = 1
        self.write(pair[1], summary)
        result = self.load(pair)
        self.assertEqual(result["groups"][0]["sample_count"], 1)
        self.assertEqual(result["groups"][0]["warmup_count"], 1)

    def test_summary_identity_count_and_small_n_p99_are_checked(self):
        for change in (dict(n=2), dict(mode="wrong-mode"), dict(p99=123)):
            with self.subTest(change=change):
                pair = self.fixture("case-" + next(iter(change)))
                summary = json.loads(pair[1].read_text())
                summary["groups"][0].update(change)
                self.write(pair[1], summary)
                with self.assertRaises(REPORT.InvalidEvidence):
                    self.load(pair)

    def test_json_and_markdown_outputs_are_explicit_new_destinations(self):
        pair = self.fixture()
        json_path = self.root / "report.json"
        md_path = self.root / "report.md"
        code = REPORT.main(["--run", str(pair[0]), "--summary", str(pair[1]),
                            "--output-json", str(json_path), "--output-markdown", str(md_path)])
        self.assertEqual(code, 2)
        self.assertEqual(json.loads(json_path.read_text())["status"], "incomplete")
        self.assertIn("Process CPU/RSS remain process-scoped", md_path.read_text())

    def test_toolset_hash_is_checked_against_actual_frozen_fixture(self):
        pair = self.fixture()
        run = json.loads(pair[0].read_text())
        manifest_path = Path(run["dataset_manifest"]["path"])
        manifest = json.loads(manifest_path.read_text())
        fixture = {"case": "tool-catalog", "profiles": {"coding": {
            "registered": ["synthetic"], "eligible": ["synthetic"],
            "visible": ["synthetic"], "schema": {"synthetic": {}}}}}
        snapshot = self.root / "tiny-snapshot"
        actual = self.write(snapshot / "fixture.json", fixture)
        manifest["snapshot"] = str(snapshot)
        manifest["files"] = [{"path": "fixture.json", "bytes": actual.stat().st_size,
                               "sha256": REPORT.sha256_file(actual)}]
        manifest["dataset_sha256"] = REPORT.canonical_sha256(manifest["files"])
        manifest["generation"].update(case="tool-catalog", fixture_facts=fixture)
        manifest["comparison"]["toolset_sha256"] = REPORT.canonical_sha256(fixture["profiles"])
        self.write(manifest_path, manifest)
        run["dataset_manifest"] = dict(self.descriptor(manifest_path), **{
            key: manifest[key] for key in ("dataset_sha256", "protocol_sha256", "fixed_workspace",
                                           "generation", "metadata", "comparison")})
        self.write(pair[0], run)
        self.rewrite_row(pair, lambda row: row.update(dataset_sha256=manifest["dataset_sha256"]))
        summary = json.loads(pair[1].read_text())
        summary["groups"][0]["dataset_sha256"] = manifest["dataset_sha256"]
        self.write(pair[1], summary)
        self.assertFalse(self.load(pair)["comparison_identity_missing"])
        self.write(actual, {"profiles": {"tampered": True}})
        with self.assertRaisesRegex(REPORT.InvalidEvidence, "byte hash mismatch"):
            self.load(pair)

    def test_aborted_receipt_stays_invalid_and_preserves_original_reason(self):
        aborted = self.write(self.root / "aborted.json", {
            "exit": -15, "reason": "SyntheticTimeout", "accepted_samples": False})
        with contextlib.redirect_stdout(io.StringIO()) as output:
            code = REPORT.main(["--run", str(aborted), "--summary", str(self.root / "not-created.json")])
        value = json.loads(output.getvalue())
        self.assertEqual(code, 1)
        self.assertEqual(value["rejected_run"]["failure_fields"]["reason"], "SyntheticTimeout")

    def test_preview_cannot_be_mixed_with_success(self):
        left = self.load(self.fixture())
        right = self.load(self.fixture("preview", outcome="preview_only"))
        self.assertEqual(REPORT.compare_runs(left, right)["status"], "not_comparable")

    def test_gui_scope_does_not_claim_native_paint_or_real_rtl(self):
        pair = self.fixture()
        run = json.loads(pair[0].read_text())
        binary = {"path": run["binary"]["path"], "sha256": run["binary"]["sha256"]}
        run.update(kind="gui", binary={"node": binary, "vitest_entry": binary})
        self.write(pair[0], run)
        value = self.load(pair)
        self.assertEqual(value["coverage_status"], "incomplete")
        self.assertIn("real_RTL_interaction_not_established", value["gaps"])
        self.assertIn("GUI_jsdom_or_other_synthetic_render_is_not_native_paint", value["gaps"])


class SnapshotCompileControls(unittest.TestCase):
    def fixture(self, root):
        roots, supplied = snapshot_fixture(root)
        def write(name, value):
            p = root / name; p.write_text(json.dumps(value) + "\n")
            return {"path": str(p), "sha256": REPORT.sha256_file(p)}
        start = json.loads(Path(supplied["eko"]["compile_start"]["path"]).read_text())
        receipt_value = {"label":"snapshot-synthetic-NoRun", "command":start["command"], "cwd":start["cwd"],
                         "logPath":str(root/"compile.raw"), "exitCode":0, "signal":None, "errorCode":None,
                         "truncated":False, "bytesSeen":0, "bytesWritten":0,
                         "startedAt":"2026-10-31T00:00:00Z", "finishedAt":"2026-10-31T00:00:01Z"}
        receipt = write("compile.json", receipt_value)
        binary = root / "synthetic-retained.bin"; binary.write_text("synthetic metadata only, not executed\n")
        binaries = {str(binary):REPORT.sha256_file(binary)}
        fp = write("fingerprint.json", {"profile":77,"features":"[]"})
        artifacts = write("artifacts.json", {"actual_NoRun_receipt":receipt, "roles":[{
            "retained_binary":{"path":str(binary),"sha256":binaries[str(binary)]}, "retained_fingerprint":fp,
            "actual_Cargo_profile_ID":77,"actual_features_string":"[]", "source_commit":supplied["eko"]["origin_commit"],
            "source_manifest":supplied["eko"]["manifest"], "original_compile_receipt":receipt}]})
        supplied["eko"]["artifacts"] = artifacts
        features = {"eko":{"default_features":True,"all_features":False,"explicit":[]}}
        final = {"receipt":receipt,"label":receipt_value["label"],"command":start["command"],"cwd":start["cwd"],
                 "repository":"eko","profile":"release","features":features,"retained_binaries":binaries}
        descriptor = {**receipt,"final_steps":[final]}
        profiles = {"profile":"release","features":features,"retained_binaries":binaries,"source_inputs":supplied,
                    "evidence_reader":{"path":str(SCRIPT.resolve()),"sha256":REPORT.sha256_file(SCRIPT)},
                    "source_identity_reader":{"path":str(Path(SCRIPT).with_name("performance-baseline-runner.py").resolve()),
                                              "sha256":REPORT.sha256_file(Path(SCRIPT).with_name("performance-baseline-runner.py"))}}
        identities = {name:RUNNER.source_identity(roots[name], supplied[name]) for name in roots}
        return {"roots":{name:str(path) for name,path in roots.items()},"descriptor":descriptor,"profiles":profiles,
                "binary":binary,"sha":binaries[str(binary)],"identities":identities,"receipt_value":receipt_value}

    def qualify(self, root, fixture):
        return REPORT.compile_qualification(root/"capsule-not-written.json", fixture["descriptor"], set(),
            fixture["profiles"], fixture["roots"], fixture["binary"], fixture["sha"], fixture["identities"])

    def test_snapshot_compile_positive_keeps_real_cwd_and_material_refs(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory); f=self.fixture(root); result=self.qualify(root,f)
            self.assertEqual(result["status"],"recognized_success")
            self.assertEqual(result["steps"][0]["step"]["cwd"],f["roots"]["eko"])
            self.assertEqual(result["source_materials"]["eko"]["head"],None)
            self.assertEqual(result["source_materials"]["framework"]["source_kind"],"git_object_snapshot")

    def test_missing_bridge_and_old_parent_git_cannot_be_accepted(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory); f=self.fixture(root); f["profiles"].pop("source_inputs")
            with self.assertRaisesRegex(REPORT.InvalidEvidence,"snapshot input missing"):
                self.qualify(root,f)

    def test_snapshot_keeps_existing_cwd_argv_profile_feature_binary_FP_rejections(self):
        for change in ("cwd","argv","profile","features","binary","FP","reader","runner","origin","manifesthash","fullset"):
            with self.subTest(change=change), tempfile.TemporaryDirectory() as directory:
                root=Path(directory); f=self.fixture(root); final=f["descriptor"]["final_steps"][0]
                supplied=f["profiles"]["source_inputs"]["eko"]
                if change=="cwd": final["cwd"]=str(root/"foreign")
                elif change=="argv": final["command"]=["cargo","test","--no-run"]
                elif change=="profile": final["profile"]="test"
                elif change=="features": f["profiles"]["features"]["eko"]["explicit"]=["unrequested"]
                elif change=="binary": final["retained_binaries"][str(f["binary"]) ]="f"*64
                elif change=="FP": (root/"fingerprint.json").write_text('{"profile":78,"features":"[]"}')
                elif change=="reader": f["profiles"]["evidence_reader"]["sha256"]="0"*64
                elif change=="runner": f["profiles"]["source_identity_reader"]["sha256"]="0"*64
                elif change=="origin": supplied["origin_commit"]="c"*40
                elif change=="manifesthash": supplied["manifest"]["sha256"]="0"*64
                else: (Path(f["roots"]["eko"])/"untracked").write_text("extra")
                with self.assertRaises((ValueError,REPORT.InvalidEvidence)):
                    self.qualify(root,f)


if __name__ == "__main__":
    unittest.main(verbosity=2)
