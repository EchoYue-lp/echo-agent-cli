"""Small correctness controls for the runner; never product performance data."""
import importlib.util
from pathlib import Path
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location("baseline_runner", Path(__file__).with_name("performance-baseline-runner.py"))
RUNNER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RUNNER)


class SnapshotOwnershipControls(unittest.TestCase):
    def test_native_units_are_separate_from_the_original_workloads(self):
        old = RUNNER.workload_units("conversation")
        self.assertNotIn("native_history_messages", old)
        for case in ("native-history-gui", "native-history-fork", "native-history-safe-point"):
            units = RUNNER.workload_units(case)
            for key, value in old.items():
                self.assertEqual(units[key], value)
            self.assertIn("canonical Message", units["native_history_messages"])
            self.assertIn("not TaskRuntime", units["native_history_events"])
            self.assertIn("attempted/successful", units["native_history_io"])

    def test_native_hot_and_cold_keep_the_existing_fixed_sample_protocol(self):
        manifest = {"dataset_sha256": "synthetic-dataset", "protocol_sha256": "synthetic-protocol"}

        def row(phase, index):
            return {"scenario": "native-history/gui-loader", "mode": "hot", "phase": phase,
                    "sample_index": index, "outcome": "success", "elapsed_ns": "1",
                    "correctness": {"ok": True}, "dataset_sha256": "synthetic-dataset",
                    "protocol_sha256": "synthetic-protocol", "source_sha256": "synthetic-source",
                    "environment_id": "synthetic-environment", "process_observation_id": "synthetic-process"}

        rows = [row("warmup", number) for number in range(5)] + [row("measurement", number) for number in range(30)]
        RUNNER.validate_rows(rows, manifest, "synthetic-source", "synthetic-environment", "synthetic-process", "hot", "measurement", 0)
        with self.assertRaisesRegex(RuntimeError, "omitted/duplicated"):
            RUNNER.validate_rows(rows[:-1], manifest, "synthetic-source", "synthetic-environment", "synthetic-process", "hot", "measurement", 0)
        cold = row("measurement", 7)
        cold["mode"] = "process-cold"
        RUNNER.validate_rows([cold], manifest, "synthetic-source", "synthetic-environment", "synthetic-process", "process-cold", "measurement", 7)

    def make_snapshot(self, root):
        workspace = root / "workspace"
        snapshot = root / "snapshot"
        with RUNNER.workspace_owner(workspace, create=True):
            workspace.mkdir()
            (workspace / "fixture.json").write_text('{"text":"中文 🧪"}\n')
            RUNNER.remember_workspace(workspace)
            RUNNER.shutil.copytree(workspace, snapshot)
        files = RUNNER.inventory(snapshot)
        return workspace, snapshot, {"fixed_workspace": str(workspace), "snapshot": str(snapshot),
                                     "files": files, "dataset_sha256": RUNNER.digest(RUNNER.encoded(files))}

    def test_same_absolute_root_reuses_exact_bytes(self):
        with tempfile.TemporaryDirectory() as directory:
            workspace, snapshot, manifest = self.make_snapshot(Path(directory))
            with RUNNER.workspace_owner(workspace):
                RUNNER.restore(workspace, snapshot, manifest)
            self.assertEqual(RUNNER.inventory(workspace), manifest["files"])
            self.assertEqual(RUNNER.inventory(snapshot), manifest["files"])

    def test_foreign_edit_or_relocation_is_retained(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            workspace, snapshot, manifest = self.make_snapshot(root)
            (workspace / "unexpected-user-file").write_text("keep")
            with self.assertRaisesRegex(ValueError, "changed outside"):
                RUNNER.restore(workspace, snapshot, manifest)
            self.assertEqual((workspace / "unexpected-user-file").read_text(), "keep")
            with self.assertRaisesRegex(ValueError, "cannot be relocated"):
                RUNNER.restore(root / "different", snapshot, manifest)

    def test_tamper_or_symlink_cannot_be_a_frozen_snapshot(self):
        with tempfile.TemporaryDirectory() as directory:
            workspace, snapshot, manifest = self.make_snapshot(Path(directory))
            (snapshot / "fixture.json").write_text("tamper")
            with self.assertRaisesRegex(ValueError, "identity changed"):
                RUNNER.restore(workspace, snapshot, manifest)
            (snapshot / "link").symlink_to(workspace / "fixture.json")
            with self.assertRaisesRegex(ValueError, "symlinks"):
                RUNNER.inventory(snapshot)

    def test_seed_never_adopts_an_existing_directory(self):
        with tempfile.TemporaryDirectory() as directory:
            workspace = Path(directory) / "existing"
            workspace.mkdir()
            with self.assertRaisesRegex(ValueError, "never adopts"):
                with RUNNER.workspace_owner(workspace, create=True):
                    self.fail("existing path must not be adopted")
            self.assertTrue(workspace.exists())

    def test_canonical_manifest_bytes_match_the_cross_language_contract(self):
        self.assertEqual(RUNNER.encoded({"b": [2, 1], "a": "中文 🧪"}),
                         '{"a":"中文 🧪","b":[2,1]}'.encode("utf-8"))


if __name__ == "__main__":
    unittest.main()
