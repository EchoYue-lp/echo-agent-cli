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
import stat
import subprocess
import sys
import time
import uuid

DIRECTORY_ANCHOR_SHA256 = "5397fbfcd06e78bd2875faacb92d149d89c129a97d71c31c22755d2f4005d409"
DIRECTORY_POSTSCAN_SHA256 = "3874f11e64fa06dbac41fac7fa95c9a1253cffe4a51be78820233c0ba0d029df"


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


def bound_document(descriptor):
    """共同的来源引用读取；缺失/损坏不会选择另一个来源。"""
    if not isinstance(descriptor, dict) or not {"path", "sha256"} <= descriptor.keys():
        raise ValueError("bound source descriptor missing")
    path = Path(descriptor["path"])
    if not path.is_absolute() or path.resolve() != path or path.is_symlink() or not path.is_file():
        raise ValueError("bound source must be an absolute regular file")
    data = path.read_bytes()
    if digest(data) != descriptor["sha256"] or ("bytes" in descriptor and len(data) != descriptor["bytes"]):
        raise ValueError("bound source hash/size mismatch")
    return json.loads(data)


def source_member(path, row, mode, git_blob=None):
    if path.is_symlink() or not path.is_file():
        raise ValueError("source member kind differs")
    data = path.read_bytes()
    if len(data) != row["bytes"] or digest(data) != row["sha256"] or stat.S_IMODE(path.stat().st_mode) != mode:
        raise ValueError("source byte/size/fullmode drift: " + row["path"])
    if git_blob is not None and hashlib.sha1(b"blob " + str(len(data)).encode() + b"\0" + data).hexdigest() != git_blob:
        raise ValueError("source Git blob drift: " + row["path"])
    return {"path": row["path"], "kind": "regular", "bytes": len(data), "sha256": digest(data)}


def source_tree(root, expected, modes, readonly, blobs=None):
    """旧/新来源共用完整集合与模式检查，不为生成物增加ignore。"""
    directories = {"."}
    for rel in expected:
        directories.update(str(p) for p in Path(rel).parents)
    actual = {}
    for path in [root, *sorted(root.rglob("*"))]:
        if path.is_symlink():
            raise ValueError("source symlink rejected")
        if path.is_dir():
            if str(path.relative_to(root)) not in directories:
                raise ValueError("extra source directory")
            if readonly and stat.S_IMODE(path.stat().st_mode) != 0o555:
                raise ValueError("readonly directory mode drift")
            continue
        rel = path.relative_to(root).as_posix()
        if rel not in expected:
            raise ValueError("extra/nonregular source entry")
        actual[rel] = source_member(path, expected[rel], modes[rel] & ~0o222 if readonly else modes[rel],
                                    blobs[rel] if blobs is not None else None)
    if set(actual) != set(expected):
        raise ValueError("missing source member")
    return [actual[key] for key in sorted(actual)]


def git_tree(root, commit):
    def read(*args):
        call = subprocess.run(["git", "-C", str(root), *args], capture_output=True)
        if call.returncode != 0:
            raise ValueError("source Git object read failed")
        return call.stdout
    if root.is_symlink() or root.resolve() != root or Path(os.fsdecode(read("rev-parse", "--show-toplevel")).strip()) != root:
        raise ValueError("source Git root differs")
    rows = {}
    for entry in read("ls-tree", "-rz", "--full-tree", commit).split(b"\0"):
        if not entry:
            continue
        header, name = entry.split(b"\t", 1)
        mode, kind, blob = header.decode().split()
        rel = os.fsdecode(name)
        if kind != "blob" or mode not in ("100644", "100755") or rel in rows:
            raise ValueError("unsupported or duplicate Git source member")
        rows[rel] = {"git_mode": mode, "git_blob": blob}
    return rows


def materialization_source_identity(root, supplied):
    if supplied.get("kind") != "git_object_snapshot" or supplied.get("repository") not in ("eko", "framework"):
        raise ValueError("explicit material source kind/repository required")
    if set(supplied) & {"git_objects", "compile_start", "permission_phase", "artifacts"}:
        raise ValueError("mixed source proof formats")
    if root.is_symlink() or root.resolve() != root or (root / ".git").exists():
        raise ValueError("actual material root must be a real non-Git snapshot")
    manifest = bound_document(supplied.get("manifest"))
    guard = bound_document(supplied.get("sourceguard"))
    if manifest.get("state") != "EXACT_080_F340_SOURCE_MATERIALIZED_NO_CARGO" or set(manifest.get("sources", {})) != {"eko", "framework"}:
        raise ValueError("unrecognized material source authority")
    repository = supplied["repository"]
    source = manifest["sources"][repository]
    result = guard.get("results", {}).get(repository, {})
    if (source.get("actual_build_source") != str(root) or source.get("commit") != supplied.get("origin_commit") or
            re.fullmatch(r"[0-9a-f]{40}", str(source.get("commit"))) is None or
            guard.get("state") not in {"PASS080_ORDER1_POST_SOURCEGUARD0", "PASS080_ORDER2_POST_SOURCEGUARD0", "PASS080_ORDER3_POST_SOURCEGUARD0"} or
            guard.get("source_manifest", {}).get("sha256") != supplied["manifest"]["sha256"] or
            result.get("commit") != source["commit"] or result.get("full_bytes_Gitblob_mode_exec_unchanged") is not True or
            result.get("formal_clean") is not True or source.get("relative_member_set_complete") is not True or source.get("Git_blob_bytes_verified") is not True):
        raise ValueError("material source/guard/origin mismatch")
    expected, modes, blobs = {}, {}, {}
    tree = git_tree(Path(source["formal_root"]), source["commit"])
    for row in source["files"]:
        rel = row.get("path")
        if (not isinstance(rel, str) or not rel or Path(rel).is_absolute() or "\\" in rel or
                any(p in (".", "..", "") for p in rel.split("/")) or rel in expected or row.get("kind") != "regular" or
                type(row.get("bytes")) is not int or row["bytes"] < 0 or re.fullmatch(r"[0-9a-f]{64}", str(row.get("sha256"))) is None or
                tree.get(rel) != {key: row.get(key) for key in ("git_mode", "git_blob")}):
            raise ValueError("material source member/object mismatch")
        expected[rel], modes[rel], blobs[rel] = row, int(row["git_mode"], 8) & 0o777, row["git_blob"]
        expected_mode = modes[rel] & ~0o222 if repository == "framework" else modes[rel]
        if row.get("actual_mode") != oct(expected_mode):
            raise ValueError("material original actual mode declaration differs")
    if set(expected) != set(tree) or len(expected) != source.get("member_count") or len(expected) != result.get("files"):
        raise ValueError("incomplete material source authority")
    readonly = repository == "framework"
    if readonly and (source.get("actual_readonly_root_and_children") is not True or source.get("writable_entries") != [] or result.get("writable_F_including_root") != []):
        raise ValueError("material readonly permission proof missing")
    files = source_tree(root, expected, modes, readonly, blobs)
    return {"root": str(root), "head": None, "status": None, "source_kind": "git_object_snapshot", "origin_commit": source["commit"],
            "source_input": supplied, "files": files, "sha256": digest(encoded({"files": files, "source_input": supplied})),
            "locks": {name: digest((root / name).read_bytes()) for name in ["Cargo.lock", "Cargo.toml", "echo-agent-app-core/Cargo.toml",
                       "web-frontend/package-lock.json", "web-frontend/package.json"] if (root / name).is_file()}}


def verify_directory_anchor(bridge, source, directories, formal, target):
    """消费Root的独立新冻结点；不产生/更新expected，不声称P1历史目录已验。"""
    descriptor=bridge.get('directory_anchor');post_descriptor=bridge.get('directory_postscan')
    if not isinstance(descriptor,dict) or not isinstance(post_descriptor,dict):
        raise ValueError('directory anchor missing: current provenance gap')
    authority=formal.parent/'.echo-semantic/local/root-full-framework-window-20261029'
    if (descriptor.get('sha256')!=DIRECTORY_ANCHOR_SHA256 or post_descriptor.get('sha256')!=DIRECTORY_POSTSCAN_SHA256 or
            descriptor.get('path')!=str(authority/'F340-dual-root-directory-freeze-20261101-r01.json') or
            post_descriptor.get('path')!=str(authority/'F340-dual-root-directory-freeze-postscan-20261101-r01.json')):
        raise ValueError('directory anchor unknown path/SHA authority')
    anchor=bound_document(descriptor);post=bound_document(post_descriptor)
    tree_call=subprocess.run(['git','--no-optional-locks','-C',str(formal),'rev-parse',source['commit']+'^{tree}'],capture_output=True)
    if (tree_call.returncode or anchor.get('state')!='ROOT_INDEPENDENT_NEW_DIRECTORY_FREEZE_POINT_NOT_HISTORICAL_COMPILE_METADATA' or
            anchor.get('formal_root')!=str(formal) or anchor.get('snapshot_root')!=str(target) or anchor.get('commit')!=source['commit'] or
            anchor.get('git_tree')!=tree_call.stdout.decode().strip() or anchor.get('file_count')!=source['member_count'] or
            anchor.get('directory_count')!=len(directories) or anchor.get('expected_directory_modes')!={'formal':'0o755','snapshot':'0o555'} or
            post.get('state')!='ROOT_NEW_FREEZE_DIRECTORY_SECOND_SCAN_EXACT0' or
            post.get('anchor')!={'path':descriptor['path'],'sha256':descriptor['sha256']} or post.get('drift')!=[] or
            post.get('actual_roots')!={'formal':str(formal),'snapshot':str(target)} or
            post.get('independently_checked_directory_count_per_root')!=len(directories) or post.get('device_inode_kind_fullmode_mtime_ctime_all_equal_anchor') is not True):
        raise ValueError('directory anchor source/tree/root/new-freeze scope differs')
    entries=anchor.get('directories',[])
    by_path={row['path']:row for row in entries}
    if len(by_path)!=len(entries) or set(by_path)!=directories:
        raise ValueError('directory anchor complete expected set differs')
    for relative,row in by_path.items():
        for name,root in (('formal',formal),('snapshot',target)):
            path=root/relative
            if path.is_symlink() or not path.is_dir() or path.resolve()!=path:
                raise ValueError('directory anchor kind/path drift')
            info=path.lstat();expected=row[name]
            actual={'path':str(path),'device':str(info.st_dev),'inode':str(info.st_ino),'mode':oct(stat.S_IMODE(info.st_mode)),
                    'mtime_ns':str(info.st_mtime_ns),'ctime_ns':str(info.st_ctime_ns)}
            if actual!=expected or actual['mode']!=anchor['expected_directory_modes'][name]:
                raise ValueError('directory anchor physical/fullmode drift: '+name+'/'+relative)
    return anchor


def scan_same_source_bridge(bridge):
    """独立重读两根完整源码；只输出当前诊断，不自行承认冻结。"""
    if bridge.get("kind") != "framework-p1-same-source-v1":
        raise ValueError("explicit same-source bridge kind required")
    supplied = bridge["qualified_source"]
    if supplied.get("repository") != "framework" or supplied.get("proof_format") != "materialization-v1":
        raise ValueError("same-source bridge qualified format differs")
    manifest = bound_document(supplied["manifest"])
    source = manifest["sources"]["framework"]
    target = Path(source["actual_build_source"])
    materialization_source_identity(target, supplied)
    index = bound_document(bridge["source_index"])
    context = bound_document(bridge["compile_context"])
    prior = bound_document(bridge["prior_freeze"])
    if (prior.get("state") != "READONLY_DEBUG_PHASE3_PER_ROLE_PROVENANCE_ROOT_CAUSE_CONFIRMED" or
            prior.get("source_manifest", {}).get("sha256") != supplied["manifest"]["sha256"] or
            prior.get("P1_source_index", {}).get("sha256") != bridge["source_index"]["sha256"]):
        raise ValueError("same-source independent prior freeze authority differs")
    prior_roots = prior["framework_shared_source_diagnostic"]
    formal = Path(index["root"])
    if (str(formal) != source["formal_root"] or context.get("cwd") != str(formal) or
            context.get("source_index_sha256") != bridge["source_index"]["sha256"] or index.get("HEAD") != source["commit"]):
        raise ValueError("same-source bridge original cwd/object authority differs")
    def current_git(*args):
        call = subprocess.run(["git", "--no-optional-locks", "-C", str(formal), *args], capture_output=True)
        if call.returncode:
            raise ValueError("same-source formal Git read failed")
        return call.stdout
    if current_git("rev-parse", "HEAD").decode().strip() != index["HEAD"] or current_git("status", "--short") != b"":
        raise ValueError("same-source formal RootF changed")
    tree = git_tree(formal, index["HEAD"])
    members = {row["path"]: row for row in index["members"]}
    expected = {row["path"]: row for row in source["files"]}
    if len(members) != len(index["members"]) or set(members) != set(expected) or set(tree) != set(expected):
        raise ValueError("same-source complete member set differs")
    observed = {"formal": [], "snapshot": []}
    content = []
    directories = {"."}
    for rel in expected:
        directories.update(str(p) for p in Path(rel).parents)
    anchor=verify_directory_anchor(bridge,source,directories,formal,target)
    anchor_files={row['path']:row for row in anchor.get('files',[])}
    if len(anchor_files)!=len(anchor.get('files',[])) or set(anchor_files)!=set(expected):
        raise ValueError('directory anchor independently checked file set differs')
    # 先查路径中的目录，避免对已漂移为symlink的父目录读取任何成员。
    for root in (formal, target):
        for rel in directories:
            path = root / rel
            if path.is_symlink() or not path.is_dir():
                raise ValueError("same-source directory identity differs")
    for rel in sorted(expected):
        row, prior = expected[rel], members[rel]
        mode = int(row["git_mode"], 8) & 0o777
        if (tree[rel] != {key: row[key] for key in ("git_mode", "git_blob")} or prior["bytes"] != row["bytes"] or
                prior["sha256"] != row["sha256"] or prior["mode"] != mode):
            raise ValueError("same-source original Git/bytes/fullmode authority differs")
        for name, root, fullmode in (("formal", formal, mode), ("snapshot", target, mode & ~0o222)):
            path = root / rel
            source_member(path, row, fullmode, row["git_blob"])
            info = path.stat()
            if (anchor_files[rel].get('git_mode')!=row['git_mode'] or anchor_files[rel].get('git_blob')!=row['git_blob'] or
                    anchor_files[rel].get(name)!={'bytes':row['bytes'],'sha256':row['sha256'],'mode':oct(fullmode),'device':str(info.st_dev),'inode':str(info.st_ino)}):
                raise ValueError('directory anchor file/Git/physical relation differs')
            observed[name].append([rel, info.st_dev, info.st_ino, fullmode, info.st_mtime_ns, info.st_ctime_ns])
        if observed["formal"][-1][1:3] == observed["snapshot"][-1][1:3]:
            raise ValueError("same-source roots unexpectedly share file inode")
        content.append({key: row[key] for key in ("path", "bytes", "sha256", "git_blob", "git_mode")})
        directories.update(str(p) for p in Path(rel).parents)
    states = {}
    for name, root in (("formal", formal), ("snapshot", target)):
        dirs = []
        for rel in sorted(directories):
            path = root / rel
            if path.is_symlink() or not path.is_dir():
                raise ValueError("same-source directory identity differs")
            info = path.stat()
            dirs.append([rel, info.st_dev, info.st_ino, stat.S_IMODE(info.st_mode), info.st_mtime_ns, info.st_ctime_ns])
        states[name] = {"root": str(root), "files_physical_sha256": digest(encoded(observed[name])), "directories_physical_sha256": digest(encoded(dirs))}
    if current_git("rev-parse", "HEAD").decode().strip() != index["HEAD"] or current_git("status", "--short") != b"":
        raise ValueError("same-source formal RootF changed during scan")
    verify_directory_anchor(bridge,source,directories,formal,target)
    formal_rows = {row[0]: row for row in observed["formal"]}
    snapshot_rows = {row[0]: row for row in observed["snapshot"]}
    physical = [[row["path"], *formal_rows[row["path"]][1:3], *formal_rows[row["path"]][4:6],
                 *snapshot_rows[row["path"]][1:3], *snapshot_rows[row["path"]][4:6]] for row in source["files"]]
    if (prior_roots.get("formal_root") != str(formal) or prior_roots.get("snapshot_root") != str(target) or
            prior_roots.get("formal_root_mode") != oct(stat.S_IMODE(formal.stat().st_mode)) or
            prior_roots.get("snapshot_root_mode") != oct(stat.S_IMODE(target.stat().st_mode)) or
            prior_roots.get("physical_inode_time_binding_sha256") != digest(encoded(physical)) or
            prior_roots.get("content_and_Gitmode_identity_sha256") != digest(encoded(content))):
        raise ValueError("same-source independent prior freeze identity changed")
    return {"origin_commit": index["HEAD"], "member_count": len(content), "Gitblob_bytes_Gitmode_exec_sha256": digest(encoded(content)),
            "directory_provenance":{"anchor":bridge['directory_anchor'],'postscan':bridge['directory_postscan'],'collected_at':anchor['at'],
                'scope':'current independent safe point onward only','P1_build_time_directory_metadata':'unknown'},
            "actual_fullmodes_compared_separately": True, "roots": states}


def verify_same_source_bridge(bridge, original_cwd, qualified_root):
    if bridge.get("frozen_state") is None:
        raise ValueError("same-source frozen state missing")
    before = scan_same_source_bridge(bridge)
    if (before != bridge["frozen_state"] or before["roots"]["formal"]["root"] != str(original_cwd) or
            before["roots"]["snapshot"]["root"] != str(qualified_root) or scan_same_source_bridge(bridge) != before):
        raise ValueError("same-source root/freeze identity changed")
    return before


def snapshot_source_identity(root, supplied):
    """同一来源权威的显式Git对象快照；无Git失败时绝不自动选择此分支。"""
    if isinstance(supplied, dict) and "proof_format" in supplied:
        if supplied["proof_format"] != "materialization-v1":
            raise ValueError("unknown explicit source proof format")
        return materialization_source_identity(root, supplied)
    def require(ok, message):
        if not ok:
            raise ValueError("source snapshot: " + message)
    def bound(descriptor):
        require(isinstance(descriptor, dict) and set(descriptor) >= {"path", "sha256"}, "bound descriptor missing")
        path = Path(descriptor["path"])
        require(path.is_absolute() and path.absolute() == path.resolve() and not path.is_symlink(), "bound manifest path must be absolute and regular")
        data = path.read_bytes()
        require(digest(data) == descriptor["sha256"], "bound manifest hash mismatch")
        return json.loads(data)
    require(isinstance(supplied, dict) and supplied.get("kind") == "git_object_snapshot", "explicit source kind required")
    repository = supplied.get("repository")
    require(repository in ("eko", "framework"), "repository missing")
    require(not root.is_symlink() and root.absolute() == root.resolve() and not (root / ".git").exists(), "actual build root must be a non-Git, non-symlink snapshot")
    manifest = bound(supplied.get("manifest"))
    objects = bound(supplied.get("git_objects"))
    started = bound(supplied.get("compile_start"))
    guarded = bound(supplied.get("sourceguard"))
    require(started.get("source_manifest", {}).get("sha256") == supplied["manifest"]["sha256"] and
            guarded.get("source_manifest_sha256") == supplied["manifest"]["sha256"], "compile source manifest not bound")
    source = manifest["formal_sources"][repository]
    proof = objects["formal_eko" if repository == "eko" else "formal_framework"]
    require(source["materialized_root"] == str(root) == proof["materialized_root"], "foreign actual root")
    require(started["cwd"] == manifest["formal_sources"]["eko"]["materialized_root"], "foreign compile cwd")
    require(re.fullmatch(r"[0-9a-f]{40}", str(supplied.get("origin_commit"))) is not None and
            source["commit"] == proof["commit"] == supplied.get("origin_commit") ==
            guarded["results"][repository]["formal_head"], "wrong origin commit")
    require(guarded.get("state") == "PASS_AFTER39C_POST_NORUN_SOURCEGUARD0" and
            guarded["results"][repository].get("bytes_and_exec_and_actual_mode_match") is True and
            proof.get("git_tree_object_bytes_verified") is True and proof.get("mode_bits_verified") is True,
            "original sourceguard/object proof not qualified")
    readonly = repository == "framework"
    if readonly:
        phase = bound(supplied.get("permission_phase"))
        prior = bound(phase.get("prior_permission_phase"))
        require(phase.get("state") == "F_SOURCE_ROOT_READONLY_SUCCESSOR_BOUND_31B" and
                phase.get("owned_F_source_root") == str(root) and phase.get("root_mode_after") == "0o555" and
                phase.get("writable_entries_after") == [] and phase.get("source_bytes_changed") is False and
                phase.get("source_exec_bits_changed") is False and
                prior.get("owned_F_source") == str(root) and prior.get("F_bytes_unchanged") is True and
                prior.get("F_exec_bits_unchanged") is True, "readonly permission phase mismatch")
    expected, modes = {}, {}
    for row in source["files"]:
        rel = row["path"]
        require(isinstance(rel, str) and rel and not Path(rel).is_absolute() and "\\" not in rel and
                all(p not in (".", "..") for p in rel.split("/")) and rel not in expected,
                "invalid or duplicate relative path")
        require(row.get("kind") == "regular" and type(row.get("bytes")) is int and row["bytes"] >= 0 and
                re.fullmatch(r"[0-9a-f]{64}", str(row.get("sha256"))) is not None, "unsupported source kind/size/hash/symlink")
        expected[rel] = row
    for row in proof["members"]:
        rel = row["path"]
        require(rel in expected and rel not in modes and row["git_mode"] in ("100644", "100755") and
                row["bytes"] == expected[rel]["bytes"] and row["sha256"] == expected[rel]["sha256"],
                "Git object proof member mismatch")
        modes[rel] = int(row["git_mode"], 8) & 0o777
        require(int(row["actual_mode"], 8) == modes[rel], "original Git object fullmode proof differs")
    require(set(modes) == set(expected) and len(expected) == proof["member_count"] and
            guarded["results"][repository]["file_count"] == len(expected) and
            digest(encoded(source["files"])) == source["files_sha256"], "incomplete source authority")
    files = source_tree(root, expected, modes, readonly)
    return {"root": str(root), "head": None, "status": None, "source_kind": "git_object_snapshot",
            "origin_commit": source["commit"], "source_input": supplied, "files": files,
            "sha256": digest(encoded({"files": files, "source_input": supplied})),
            "locks": {name: digest((root / name).read_bytes()) for name in
                      ["Cargo.lock", "Cargo.toml", "echo-agent-app-core/Cargo.toml",
                       "web-frontend/package-lock.json", "web-frontend/package.json"] if (root / name).is_file()}}


def source_identity(root, source_input=None):
    if source_input is not None:
        return snapshot_source_identity(root, source_input)
    root = root.resolve()
    discovered = subprocess.check_output(["git", "-C", str(root), "rev-parse", "--show-toplevel"], text=True).strip()
    if Path(discovered).resolve() != root:
        raise ValueError("source Git root differs from requested root; explicit snapshot input required")
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


def verify_source_record(record, profiles=None):
    if record.get("source_input") is not None or record.get("source_kind") == "git_object_snapshot":
        if record.get("source_kind") != "git_object_snapshot":
            raise ValueError("explicit snapshot source kind missing")
        if profiles is not None:
            supplied = profiles.get("source_inputs", {})
            repository = record.get("source_input", {}).get("repository")
            if supplied.get(repository) != record.get("source_input"):
                raise ValueError("source input differs from captured profile descriptor")
    current = source_identity(Path(record["root"]), record.get("source_input"))
    if current["sha256"] != record["sha256"] or (record.get("source_kind") == "git_object_snapshot" and current != record):
        raise ValueError("source changed after qualified source identity")
    return current


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


def qualify_before_child(source_path, profiles, source_records, binary=None):
    """消费前复用唯一reader，整份视图含P1动态桥；不建立第二校验权威。"""
    path = Path(__file__).with_name("performance-baseline-report.py")
    descriptor = profiles.get("evidence_reader")
    if not isinstance(descriptor, dict) or descriptor.get("path") != str(path) or descriptor.get("sha256") != digest(path.read_bytes()):
        raise ValueError("consumer bound reader differs")
    import importlib.util
    spec = importlib.util.spec_from_file_location("consumer_bound_reader", path)
    reader = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(reader)
    if binary is not None and profiles["retained_binaries"].get(str(binary)) != digest(binary.read_bytes()):
        raise ValueError("consumer program differs from exact role inventory")
    result = reader.compile_qualification(source_path, profiles["compile_receipt"], set(), profiles,
        {name: source_records[name]["root"] for name in ("framework", "eko")}, source_records=source_records)
    if result["status"] != "recognized_success":
        raise ValueError("consumer role provenance incomplete: " + str(result.get("gap")))
    return result


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
        verify_source_record(qualified[name], qualified["profiles"])
    if qualified["profiles"]["retained_binaries"].get(str(args.binary.resolve())) != digest(args.binary.read_bytes()):
        raise ValueError("seed executable differs from qualified retained binary")
    qualify_before_child(args.metadata, qualified["profiles"], {name: qualified[name] for name in ("framework", "eko")}, args.binary.resolve())
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
                qualify_before_child(args.metadata, qualified["profiles"], {name: qualified[name] for name in ("framework", "eko")}, binary)
                child(binary, step["test"], workspace, {**env, "EKO_PERF_ACTION": step["action"]}, args.output / ("post-" + str(ordinal)))
        # Validation stays before freezing; runtime-history validation deliberately
        # does not repair discardable checkpoint variants before cold sampling.
        qualify_before_child(args.metadata, qualified["profiles"], {name: qualified[name] for name in ("framework", "eko")}, args.binary.resolve())
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
        verify_source_record(source[name], source["profiles"])
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
            qualify_before_child(args.source, source["profiles"], {name: source[name] for name in ("framework", "eko")}, args.binary.resolve())
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
        verify_source_record(source[name], source["profiles"])
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
        qualify_before_child(args.source, source["profiles"], {name: source[name] for name in ("framework", "eko")})
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
        profiles = json.loads(args.profile_metadata.read_text())
        supplied = profiles.get("source_inputs", {})
        if not isinstance(supplied, dict) or set(supplied) - {"eko", "framework"}:
            raise ValueError("invalid explicit source input map")
        f = source_identity(args.framework, supplied.get("framework")); e = source_identity(args.eko, supplied.get("eko"))
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
