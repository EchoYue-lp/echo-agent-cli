"""Native fixtures for the exact one-shot helper, on self-owned local files.

No SSH, user configuration, installation, credentials or remote files are used.
The optional EKO_CAS_FIXTURE_* controls patch only this test's child wrapper;
they never alter the production helper or its request format.
"""

import base64
import errno
import fcntl
import hashlib
import importlib.util
import json
import os
import pathlib
import selectors
import stat
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

HELPER_PATH = pathlib.Path(__file__).with_name("cas.py")
HELPER_SOURCE = HELPER_PATH.read_text(encoding="utf-8")
SPEC = importlib.util.spec_from_file_location("eko_project_cas_fixture", HELPER_PATH)
HELPER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(HELPER)

# The wrapper observes a real flock attempt, then calls the production helper.
# Negative controls deliberately break that one boundary to prove the fixtures
# detect lost serialization or a prefix-only hash. They are test-only argv.
CHILD = r"""
import importlib.util, os, sys
spec = importlib.util.spec_from_file_location("eko_cas_child", sys.argv[1])
helper = importlib.util.module_from_spec(spec)
spec.loader.exec_module(helper)
marker = int(sys.argv[2])
disable_lock = sys.argv[3] == "1"
prefix_hash = sys.argv[4] == "1"
original_flock = helper.fcntl.flock
def observed_flock(fd, operation):
    if operation == helper.fcntl.LOCK_EX and marker >= 0:
        os.write(marker, b"L")
    if not disable_lock:
        return original_flock(fd, operation)
helper.fcntl.flock = observed_flock
if prefix_hash:
    original_sha256 = helper.hashlib.sha256
    class PrefixDigest:
        def __init__(self):
            self.inner = original_sha256()
            self.seen = False
        def update(self, block):
            if not self.seen:
                self.inner.update(block)
                self.seen = True
        def hexdigest(self):
            return self.inner.hexdigest()
    helper.hashlib.sha256 = PrefixDigest
sys.argv = [sys.argv[1]]
helper.main()
"""


class CasHelperTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="eko-cas-fixture-")
        self.root = pathlib.Path(self.temporary.name) / "项目 it's 🦀"
        self.root.mkdir()
        self.target = self.root / "content.bin"
        self.lock_path = self.root / ".content.bin.lock"

    def tearDown(self):
        self.temporary.cleanup()

    def request(self, expected, replacement, path=None):
        return {
            "path": str(path or self.target),
            "expected_sha256": hashlib.sha256(expected).hexdigest(),
            "replacement_base64": base64.b64encode(replacement).decode("ascii"),
        }

    def child(self, request, marker=-1):
        command = [sys.executable, "-I", "-S", "-B", "-c", CHILD, str(HELPER_PATH), str(marker),
                   os.environ.get("EKO_CAS_FIXTURE_DISABLE_LOCK", "0"),
                   os.environ.get("EKO_CAS_FIXTURE_PREFIX_HASH", "0")]
        process = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                   stderr=subprocess.PIPE, pass_fds=(() if marker < 0 else (marker,)))
        process.stdin.write(json.dumps(request).encode("utf-8"))
        process.stdin.close()
        process.stdin = None
        return process

    def finish(self, process):
        stdout, stderr = process.communicate(timeout=10)
        self.assertEqual(process.returncode, 0, stderr.decode("utf-8", "replace"))
        self.assertEqual(stderr, b"")
        return json.loads(stdout.decode("utf-8"))

    def run_helper(self, request):
        process = self.child(request)
        try:
            return self.finish(process)
        finally:
            if process.poll() is None:
                process.kill()
                process.communicate(timeout=10)

    def wait_marker(self, descriptor):
        with selectors.DefaultSelector() as selector:
            selector.register(descriptor, selectors.EVENT_READ)
            self.assertTrue(selector.select(timeout=10), "helper did not reach the real flock call")
        self.assertEqual(os.read(descriptor, 1), b"L")

    def release_process(self, process):
        if process.poll() is None:
            process.kill()
        process.communicate(timeout=10)

    def test_probe_is_read_only_and_reports_only_primitive_availability(self):
        before = list(self.root.iterdir())
        result = subprocess.run([sys.executable, "-I", "-S", "-B", "-c", HELPER_SOURCE, "--probe"],
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=10)
        self.assertEqual(result.returncode, 0)
        self.assertEqual(json.loads(result.stdout), {"result": "available"})
        self.assertEqual(list(self.root.iterdir()), before)

    def test_unicode_binary_replacement_preserves_mode_and_keeps_persistent_sidecar(self):
        previous = "原始内容 🦀\0".encode("utf-8")
        replacement = b"'\n$(touch should-not-run)\0\xff"
        self.target.write_bytes(previous)
        self.target.chmod(0o640)
        reply = self.run_helper(self.request(previous, replacement))
        self.assertEqual(reply, {"result": "replaced"})
        self.assertEqual(self.target.read_bytes(), replacement)
        self.assertEqual(stat.S_IMODE(self.target.stat().st_mode), 0o640)
        self.assertTrue(self.lock_path.is_file())
        self.assertFalse((self.root / "should-not-run").exists())
        self.assertFalse(list(self.root.glob(".content.bin.*.tmp")))

    def test_conflict_never_replaces_the_target(self):
        self.target.write_bytes(b"current")
        identity = self.target.stat().st_ino
        reply = self.run_helper(self.request(b"stale", b"must-not-be-written"))
        self.assertEqual(reply, {"result": "conflict"})
        self.assertEqual(self.target.read_bytes(), b"current")
        self.assertEqual(self.target.stat().st_ino, identity)

    def test_hash_covers_tail_beyond_the_first_read_chunk(self):
        prefix = b"x" * (128 * 1024)
        self.target.write_bytes(prefix + "必须比较的尾部 🦀".encode("utf-8"))
        reply = self.run_helper(self.request(prefix, b"must-not-replace-a-prefix-match"))
        self.assertEqual(reply, {"result": "conflict"})
        self.assertTrue(self.target.read_bytes().endswith("必须比较的尾部 🦀".encode("utf-8")))

    def test_external_cooperating_writer_is_read_after_the_same_sidecar_lock(self):
        self.target.write_bytes(b"before")
        read_marker, write_marker = os.pipe()
        with open(self.lock_path, "a+b") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            process = self.child(self.request(b"before", b"incorrect-overwrite"), write_marker)
            os.close(write_marker)
            try:
                self.wait_marker(read_marker)
                try:
                    process.wait(timeout=0.15)
                except subprocess.TimeoutExpired:
                    pass
                else:
                    self.fail("CAS helper finished while the external cooperating sidecar lock was held")
                self.target.write_bytes(b"external-cooperating-write")
                fcntl.flock(lock, fcntl.LOCK_UN)
                self.assertEqual(self.finish(process), {"result": "conflict"})
                self.assertEqual(self.target.read_bytes(), b"external-cooperating-write")
            finally:
                fcntl.flock(lock, fcntl.LOCK_UN)
                os.close(read_marker)
                self.release_process(process)

    def test_competing_processes_get_exactly_one_replacement(self):
        self.target.write_bytes(b"initial")
        markers = [os.pipe(), os.pipe()]
        processes = []
        with open(self.lock_path, "a+b") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            try:
                for index, replacement in enumerate([b"winner-A", b"winner-B"]):
                    read_marker, write_marker = markers[index]
                    processes.append(self.child(self.request(b"initial", replacement), write_marker))
                    os.close(write_marker)
                    self.wait_marker(read_marker)
                fcntl.flock(lock, fcntl.LOCK_UN)
                replies = [self.finish(process)["result"] for process in processes]
                self.assertEqual(sorted(replies), ["conflict", "replaced"])
                self.assertIn(self.target.read_bytes(), [b"winner-A", b"winner-B"])
            finally:
                fcntl.flock(lock, fcntl.LOCK_UN)
                for read_marker, _ in markers:
                    os.close(read_marker)
                for process in processes:
                    self.release_process(process)

    def test_directory_sync_error_is_not_misreported_as_unchanged(self):
        self.target.write_bytes(b"old")
        original = HELPER.os.fsync
        def failing_directory_sync(descriptor):
            if stat.S_ISDIR(os.fstat(descriptor).st_mode):
                raise OSError(errno.EIO, "SECRET error text must not be emitted")
            return original(descriptor)
        with mock.patch.object(HELPER.os, "fsync", failing_directory_sync):
            reply = HELPER.compare_and_swap(self.request(b"old", b"new"))
        self.assertEqual(reply["result"], "error")
        self.assertEqual(reply["phase"], "directory_sync")
        self.assertTrue(reply["may_have_changed"])
        self.assertEqual(self.target.read_bytes(), b"new")
        self.assertNotIn("SECRET", json.dumps(reply))
        self.assertEqual(self.run_helper(self.request(b"new", b"after-error")), {"result": "replaced"})

    def test_unlock_error_remains_an_error_after_replacement(self):
        self.target.write_bytes(b"old")
        original = HELPER.fcntl.flock
        def failing_unlock(descriptor, operation):
            if operation == fcntl.LOCK_UN:
                raise OSError(errno.EIO, "SECRET unlock text")
            return original(descriptor, operation)
        with mock.patch.object(HELPER.fcntl, "flock", failing_unlock):
            reply = HELPER.compare_and_swap(self.request(b"old", b"new"))
        self.assertEqual(reply["result"], "error")
        self.assertEqual(reply["phase"], "unlock")
        self.assertTrue(reply["may_have_changed"])
        self.assertEqual(self.target.read_bytes(), b"new")
        self.assertEqual(self.run_helper(self.request(b"new", b"released")), {"result": "replaced"})

    def test_file_sync_error_preserves_primary_error_and_original_target(self):
        self.target.write_bytes(b"original")
        original_sync = HELPER.os.fsync
        original_flock = HELPER.fcntl.flock
        def failing_sync(descriptor):
            if stat.S_ISREG(os.fstat(descriptor).st_mode):
                raise OSError(errno.ENOSPC, "SECRET file sync text")
            return original_sync(descriptor)
        def failing_unlock(descriptor, operation):
            if operation == fcntl.LOCK_UN:
                raise OSError(errno.EIO, "secondary unlock error")
            return original_flock(descriptor, operation)
        with mock.patch.object(HELPER.os, "fsync", failing_sync), mock.patch.object(HELPER.fcntl, "flock", failing_unlock):
            reply = HELPER.compare_and_swap(self.request(b"original", b"new"))
        self.assertEqual(reply["result"], "error")
        self.assertEqual(reply["phase"], "file_sync")
        self.assertEqual(reply["errno"], errno.ENOSPC)
        self.assertFalse(reply["may_have_changed"])
        self.assertEqual(self.target.read_bytes(), b"original")
        self.assertFalse(list(self.root.glob(".content.bin.*.tmp")))

    def test_foreign_temporary_collision_is_never_deleted(self):
        self.target.write_bytes(b"original")
        foreign = self.root / ".content.bin.fixed.tmp"
        foreign.write_bytes(b"keep foreign")
        with mock.patch.object(HELPER.uuid, "uuid4", return_value=type("Fixed", (), {"hex": "fixed"})()):
            reply = HELPER.compare_and_swap(self.request(b"original", b"new"))
        self.assertEqual(reply["result"], "error")
        self.assertEqual(reply["phase"], "temporary_open")
        self.assertEqual(foreign.read_bytes(), b"keep foreign")
        self.assertEqual(self.target.read_bytes(), b"original")

    def test_missing_target_is_an_error_not_a_conflict_or_new_file(self):
        reply = self.run_helper(self.request(b"", b"must-not-create"))
        self.assertEqual(reply["result"], "error")
        self.assertEqual(reply["phase"], "read")
        self.assertFalse(self.target.exists())

    def test_unavailable_native_primitive_does_not_touch_files(self):
        self.target.write_bytes(b"keep")
        with mock.patch.object(HELPER, "fcntl", None):
            reply = HELPER.compare_and_swap(self.request(b"keep", b"new"))
        self.assertEqual(reply, {"result": "unsupported"})
        self.assertEqual(self.target.read_bytes(), b"keep")
        self.assertFalse(self.lock_path.exists())

    def test_invalid_input_is_bounded_and_does_not_echo_content(self):
        marker = b"PRIVATE_FIXTURE_MARKER"
        for raw in [marker, b"x" * (HELPER.MAX_REQUEST_BYTES + 1)]:
            process = subprocess.run([sys.executable, "-I", "-S", "-B", "-c", HELPER_SOURCE],
                                     input=raw, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=10)
            self.assertEqual(process.returncode, 0)
            self.assertEqual(json.loads(process.stdout), {"result": "invalid"})
            self.assertNotIn(marker, process.stdout + process.stderr)
        self.assertEqual(list(self.root.iterdir()), [])


if __name__ == "__main__":
    unittest.main()
