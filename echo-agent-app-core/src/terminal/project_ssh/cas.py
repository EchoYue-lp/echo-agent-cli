"""One-shot POSIX equivalent of the original cooperating-writer file CAS.

The fixed program is sent through the existing SSH executor; only request data
uses stdin. It is not installed as a server or exposed as another agent tool.
Never print paths, input bytes, complete config or exception messages.
"""

import base64
import errno
import hashlib
import json
import os
import stat
import sys
import uuid

try:
    import fcntl
except ImportError:
    fcntl = None

MAX_REQUEST_BYTES = 8 * 1024 * 1024


def capabilities():
    # Import/primitive availability is permission to attempt this operation,
    # not qualification of a particular remote filesystem or server.
    return (
        os.name == "posix"
        and fcntl is not None
        and callable(getattr(fcntl, "flock", None))
        and callable(getattr(os, "replace", None))
        and callable(getattr(os, "fsync", None))
        and callable(getattr(os, "fchmod", None))
    )


def error_result(phase, error, may_have_changed):
    return {
        "result": "error",
        "phase": phase,
        "errno": getattr(error, "errno", None),
        "may_have_changed": may_have_changed,
    }


def validate_request(request):
    if not isinstance(request, dict):
        raise ValueError("request is not an object")
    path = request.get("path")
    expected = request.get("expected_sha256")
    encoded = request.get("replacement_base64")
    if not isinstance(path, str) or not path or "\0" in path or not os.path.isabs(path):
        raise ValueError("path is not an absolute target path")
    if not isinstance(expected, str) or len(expected) != 64 or any(
        character not in "0123456789abcdef" for character in expected
    ):
        raise ValueError("expected digest is invalid")
    if not isinstance(encoded, str):
        raise ValueError("replacement is not encoded bytes")
    replacement = base64.b64decode(encoded, validate=True)
    return path, expected, replacement


def sync_parent(parent):
    descriptor = os.open(parent, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0))
    primary = None
    try:
        os.fsync(descriptor)
    except Exception as error:
        primary = error
    try:
        os.close(descriptor)
    except Exception as error:
        if primary is None:
            primary = error
    if primary is not None:
        raise primary


def compare_and_swap(request):
    if not capabilities():
        return {"result": "unsupported"}
    try:
        path, expected, replacement = validate_request(request)
    except Exception:
        return {"result": "invalid"}

    parent = os.path.dirname(path)
    filename = os.path.basename(path) or "data"
    lock_path = os.path.join(parent, "." + filename + ".lock")
    lock_descriptor = None
    temporary_descriptor = None
    temporary_path = None
    owns_temporary = False
    locked = False
    may_have_changed = False
    response = None
    phase = "create_parent"
    try:
        os.makedirs(parent, exist_ok=True)
        phase = "open_lock"
        # Match fs2's persistent .<filename>.lock protocol exactly. Never
        # unlink this sidecar: another cooperating process may hold its inode.
        lock_descriptor = os.open(lock_path, os.O_CREAT | os.O_RDWR, 0o666)
        phase = "lock"
        fcntl.flock(lock_descriptor, fcntl.LOCK_EX)
        locked = True

        phase = "read"
        digest = hashlib.sha256()
        with open(path, "rb") as source:
            while True:
                block = source.read(128 * 1024)
                if not block:
                    break
                digest.update(block)
        if digest.hexdigest() != expected:
            response = {"result": "conflict"}
        else:
            try:
                permissions = stat.S_IMODE(os.stat(path).st_mode)
            except OSError:
                # The original atomic_write preserves mode when available;
                # it does not promise inode/owner/ACL or external-writer fencing.
                permissions = None
            temporary_path = os.path.join(
                parent, "." + filename + "." + uuid.uuid4().hex + ".tmp"
            )
            phase = "temporary_open"
            temporary_descriptor = os.open(
                temporary_path, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o666
            )
            owns_temporary = True
            if permissions is not None:
                phase = "permissions"
                os.fchmod(temporary_descriptor, permissions)
            phase = "write"
            remaining = memoryview(replacement)
            while remaining:
                written = os.write(temporary_descriptor, remaining)
                if written <= 0:
                    raise OSError(errno.EIO, "short temporary-file write")
                remaining = remaining[written:]
            phase = "file_sync"
            os.fsync(temporary_descriptor)
            phase = "temporary_close"
            os.close(temporary_descriptor)
            temporary_descriptor = None
            phase = "replace"
            # Treat errors from this point conservatively; rename can already
            # have published before a later directory-sync/unlock failure.
            may_have_changed = True
            os.replace(temporary_path, path)
            owns_temporary = False
            temporary_path = None
            phase = "directory_sync"
            sync_parent(parent)
            response = {"result": "replaced"}
    except Exception as error:
        response = error_result(phase, error, may_have_changed)
        if temporary_descriptor is not None:
            try:
                os.close(temporary_descriptor)
            except OSError:
                pass
        if owns_temporary and temporary_path is not None:
            try:
                os.unlink(temporary_path)
            except FileNotFoundError:
                pass
            except OSError as cleanup_error:
                response["cleanup_errno"] = cleanup_error.errno
    finally:
        if lock_descriptor is not None:
            if locked:
                try:
                    fcntl.flock(lock_descriptor, fcntl.LOCK_UN)
                except Exception as error:
                    if response is None or response.get("result") != "error":
                        response = error_result("unlock", error, may_have_changed)
            try:
                os.close(lock_descriptor)
            except Exception as error:
                if response is None or response.get("result") != "error":
                    response = error_result("close_lock", error, may_have_changed)
    return response


def main():
    if sys.argv[1:] == ["--probe"]:
        response = {"result": "available" if capabilities() else "unsupported"}
    elif sys.argv[1:]:
        response = {"result": "invalid"}
    else:
        try:
            raw = sys.stdin.buffer.read(MAX_REQUEST_BYTES + 1)
            if len(raw) > MAX_REQUEST_BYTES:
                raise ValueError("request exceeds bounded input")
            response = compare_and_swap(json.loads(raw.decode("utf-8")))
        except Exception:
            response = {"result": "invalid"}
    # A lost result is not replayed or reconstructed from terminal output. The
    # existing SSH owner reports a possible effect if this response is lost.
    sys.stdout.write(json.dumps(response, separators=(",", ":")) + "\n")
    sys.stdout.flush()


if __name__ == "__main__":
    main()
