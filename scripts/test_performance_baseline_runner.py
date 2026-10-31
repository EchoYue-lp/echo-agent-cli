"""Small correctness controls for the runner; never product performance data."""
import importlib.util
import json
import os
import subprocess
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


def snapshot_fixture(root):
    """微型合成来源证明，不是本项目真实commit/compile或性能证据。"""
    root.mkdir(parents=True, exist_ok=True)
    def write(name, value):
        p = root / name
        p.write_text(json.dumps(value) + "\n", encoding="utf-8")
        return {"path": str(p), "sha256": RUNNER.digest(p.read_bytes())}
    roots = {name: root / name for name in ("eko", "framework")}
    sources, objects, results = {}, {}, {}
    for name, path in roots.items():
        path.mkdir()
        (path / "Cargo.toml").write_text("# SYNTHETIC source fixture\n")
        (path / "source.rs").write_text("// 中文 🧪\n")
        (path / "Cargo.toml").chmod(0o644)
        (path / "source.rs").chmod(0o755)
        files = [{"path": p.name, "kind": "regular", "bytes": p.stat().st_size,
                  "sha256": RUNNER.digest(p.read_bytes())} for p in sorted(path.iterdir())]
        commit = ("a" if name == "eko" else "b") * 40
        sources[name] = {"commit": commit, "materialized_root": str(path), "files": files,
                         "files_sha256": RUNNER.digest(RUNNER.encoded(files))}
        objects["formal_eko" if name == "eko" else "formal_framework"] = {
            "commit": commit, "materialized_root": str(path), "member_count": len(files),
            "git_tree_object_bytes_verified": True, "mode_bits_verified": True,
            "members": [{**f, "git_mode": "100644" if f["path"] == "Cargo.toml" else "100755",
                         "actual_mode": "0o644" if f["path"] == "Cargo.toml" else "0o755"} for f in files]}
        results[name] = {"formal_head": commit, "file_count": len(files), "bytes_and_exec_and_actual_mode_match": True}
    manifest = write("manifest.json", {"formal_sources": sources})
    obj = write("objects.json", objects)
    command = ["cargo", "test", "-p", "echo-agent-app-core", "--release", "--lib", "--test", "performance_baseline", "--locked", "--no-run"]
    start = write("start.json", {"cwd": str(roots["eko"]), "command": command, "source_manifest": manifest})
    guard = write("guard.json", {"state": "PASS_AFTER39C_POST_NORUN_SOURCEGUARD0", "source_manifest_sha256": manifest["sha256"], "results": results})
    prior = write("prior.json", {"owned_F_source": str(roots["framework"]), "F_bytes_unchanged": True, "F_exec_bits_unchanged": True})
    phase = write("phase.json", {"state": "F_SOURCE_ROOT_READONLY_SUCCESSOR_BOUND_31B", "prior_permission_phase": prior,
                               "owned_F_source_root": str(roots["framework"]), "root_mode_after": "0o555",
                               "writable_entries_after": [], "source_bytes_changed": False, "source_exec_bits_changed": False})
    for p in roots["framework"].iterdir():
        p.chmod(0o444 if p.name == "Cargo.toml" else 0o555)
    roots["framework"].chmod(0o555)
    supplied = {name: {"kind": "git_object_snapshot", "repository": name, "origin_commit": sources[name]["commit"],
                      "manifest": manifest, "git_objects": obj, "compile_start": start, "sourceguard": guard,
                      **({"permission_phase": phase} if name == "framework" else {})} for name in roots}
    return roots, supplied


class ExplicitSourceIdentityControls(unittest.TestCase):
    def test_snapshot_preserves_material_and_actual_build_root(self):
        with tempfile.TemporaryDirectory() as directory:
            roots, supplied = snapshot_fixture(Path(directory))
            for name in roots:
                identity = RUNNER.source_identity(roots[name], supplied[name])
                self.assertEqual(identity["head"], None)
                self.assertEqual(identity["origin_commit"], supplied[name]["origin_commit"])
                self.assertEqual(identity["root"], str(roots[name]))
                self.assertEqual(identity["source_kind"], "git_object_snapshot")
                self.assertEqual(len(identity["files"]), 2)
                RUNNER.verify_source_record(identity)
                RUNNER.verify_source_record(identity, {"source_inputs":supplied})
                with self.assertRaisesRegex(ValueError, "captured profile descriptor"):
                    RUNNER.verify_source_record(identity, {"source_inputs":{}})

    def test_snapshot_material_descriptor_is_inside_identity_hash(self):
        with tempfile.TemporaryDirectory() as directory:
            roots, supplied = snapshot_fixture(Path(directory))
            first = RUNNER.source_identity(roots["eko"], supplied["eko"])
            alternate = dict(supplied["eko"], diagnostic_receipt="explicit different material reference")
            second = RUNNER.source_identity(roots["eko"], alternate)
            self.assertNotEqual(first["sha256"], second["sha256"])

    def test_strict_snapshot_rejections(self):
        for change in ("bytes", "size", "missing", "extra", "extra-directory", "mode", "symlink", "root", "commit", "manifesthash", "kind", "compilecwd", "duplicate", "unsafe-path"):
            with self.subTest(change=change), tempfile.TemporaryDirectory() as directory:
                roots, supplied = snapshot_fixture(Path(directory))
                root, descriptor = roots["eko"], supplied["eko"]
                source = root / "source.rs"
                if change == "bytes": source.write_text("// changed\n")
                elif change == "size": source.write_bytes(source.read_bytes() + b"x")
                elif change == "missing": source.unlink()
                elif change == "extra": (root / "extra").write_text("untracked")
                elif change == "extra-directory": (root / "empty-extra").mkdir()
                elif change == "mode": source.chmod(0o644)
                elif change == "symlink": source.unlink(); source.symlink_to(Path(directory) / "manifest.json")
                elif change == "root": root = Path(directory)
                elif change == "commit": descriptor["origin_commit"] = "c" * 40
                elif change == "manifesthash": descriptor["manifest"] = dict(descriptor["manifest"], sha256="0" * 64)
                elif change == "kind": descriptor["kind"] = "implicit-fallback"
                elif change == "compilecwd":
                    p = Path(descriptor["compile_start"]["path"]); value = json.loads(p.read_text()); value["cwd"] = str(Path(directory)/"foreign"); p.write_text(json.dumps(value)); descriptor["compile_start"]["sha256"] = RUNNER.digest(p.read_bytes())
                elif change in ("duplicate", "unsafe-path"):
                    p = Path(descriptor["manifest"]["path"]); value = json.loads(p.read_text())
                    files = value["formal_sources"]["eko"]["files"]
                    if change == "duplicate": files.append(dict(files[0]))
                    else: files[0]["path"] = "../escape"
                    value["formal_sources"]["eko"]["files_sha256"] = RUNNER.digest(RUNNER.encoded(files))
                    p.write_text(json.dumps(value)); descriptor["manifest"]["sha256"] = RUNNER.digest(p.read_bytes())
                    for key in ("compile_start", "sourceguard"):
                        p = Path(descriptor[key]["path"]); value = json.loads(p.read_text())
                        if key == "compile_start": value["source_manifest"] = descriptor["manifest"]
                        else: value["source_manifest_sha256"] = descriptor["manifest"]["sha256"]
                        p.write_text(json.dumps(value)); descriptor[key]["sha256"] = RUNNER.digest(p.read_bytes())
                with self.assertRaises((ValueError, OSError)):
                    RUNNER.source_identity(root, descriptor)

    def test_readonly_phase_does_not_ignore_write_bit_or_root(self):
        for change in ("file-write", "root-write", "exec"):
            with self.subTest(change=change), tempfile.TemporaryDirectory() as directory:
                roots, supplied = snapshot_fixture(Path(directory))
                fw = roots["framework"]
                if change == "file-write": (fw / "Cargo.toml").chmod(0o644)
                elif change == "root-write": fw.chmod(0o755)
                else: (fw / "source.rs").chmod(0o444)
                with self.assertRaises(ValueError): RUNNER.source_identity(fw, supplied["framework"])

    def test_original_git_positive_and_parent_git_is_not_a_fallback(self):
        # 真正的合成临时Git仓库；不为任何39c owned源码创建.git或冒充其历史。
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            subprocess.run(["git", "init", "--quiet", str(root)], check=True)
            (root / "source.txt").write_text("synthetic Git positive\n")
            subprocess.run(["git", "-C", str(root), "add", "source.txt"], check=True)
            subprocess.run(["git", "-C", str(root), "-c", "commit.gpgsign=false", "-c", "user.name=Control", "-c", "user.email=control@example.invalid", "commit", "--quiet", "-m", "synthetic fixture"], check=True)
            original = RUNNER.source_identity(root)
            self.assertEqual(len(original["files"]), 1)
            self.assertTrue(original["head"])
            inner = root / "owned"; inner.mkdir()
            with self.assertRaisesRegex(ValueError, "Git root differs"):
                RUNNER.source_identity(inner)


if __name__ == "__main__":
    unittest.main()
