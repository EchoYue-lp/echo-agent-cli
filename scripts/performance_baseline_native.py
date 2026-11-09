"""原性能reader的唯一纯capture/fixture合同。

仅作引用、wire形状和原事实校验；不启动进程或计算性能分布。
App/probe/资源计数与一次性producer不属于此正式模块。
"""
from datetime import datetime
from pathlib import Path
import math
import re

OPERATIONS = ("mount", "single-row", "batch", "run-state")

PROTOCOL_SHA = "0b880f96ab8cbbb8794d054d2e9527893c0fa93830e6f1bdcd17830961026cf9"

CHAT_PROTOCOL_SHA = "3dc8bc5c4477bd6c8979f984dbc02b2f3e9ba7c4f1d730408d1d035905774b4c"

FROZEN_GUI_BYTES = {
    1: "8a9e7d9081ea8ba1cc0e8676d297f9ddd7efd1e8086822e6923a00fbd9883b47",
    10: "89d526cbb339566f1fccddbe474db383211078aafdbce7331f5e6e1c21adcef3",
    50: "99629db908638bf4b1ea1f8217bdd0d3c0ee6589c7518482b34bd4530b52a84b",
    100: "3e7754fe8a1c757f0282b26899d9d8046e81c12e537499c68af730db6cc59f13",
}

def absolute_file(api, parent, descriptor, inputs):
    api.require(isinstance(descriptor, dict) and isinstance(descriptor.get("path"), str) and
                Path(descriptor["path"]).is_absolute(), "native artifact descriptors must be absolute across source/run documents")
    return api.bound_file(parent, descriptor, inputs)

def qualification_document(api, path, inputs):
    """A referenced document is not covered by recursive descriptor checks."""
    resolved, value = api.document(path, inputs)
    api.require(not api.schema_example(value), "native qualification JSON is schema_only / EXAMPLE_ONLY: " + str(path))
    return resolved, value

def _actual_binding(api, final, command, cwd):
    api.require(isinstance(command, list) and command and all(isinstance(item, str) and "\0" not in item for item in command),
                "native actual command argv missing")
    api.nonempty(cwd, "native actual cwd")
    api.require(command == final.get("command") and cwd == final.get("cwd"),
                "native actual final command/cwd mismatch")

def _small_window_step(api, receipt, final):
    """Recognize real historical0 without inventing absent owner facts."""
    _actual_binding(api, final, receipt.get("command"), receipt.get("cwd"))
    if "id" in receipt:
        api.require(receipt["id"] == final.get("label"), "native small-window step identity differs")
    api.require(type(receipt.get("exit_code")) is int and receipt["exit_code"] == 0 and
                receipt.get("natural_exit") is True and receipt.get("signal") is None and
                "abort_reason" not in receipt, "native small-window command failed/aborted")
    warnings = receipt.get("warning_lines", receipt.get("warnings"))
    api.require(warnings == [], "native small-window command warning facts absent/nonzero")
    # Original frontend/probe records do not assert all these facts. A later
    # descriptor/sidecar cannot retroactively supply original owner assertions.
    missing = [name for name in ("wrapperExitCode", "allDirectChildAwaited", "allOwnedDescendantsConfirmedSettled",
                                "rawStreamsFinal", "truncated", "stdoutSha256", "stderrSha256") if name not in receipt]
    api.require(False, "native small-window receipt recognized with original natural0; full qualification unavailable: " +
                ("missing original owner/stream facts " + ",".join(missing) if missing else "unsupported mixed owner schema"))

def _ops_step(api, parent, receipt, final, inputs):
    """Internal lossless view of actual shared-ops facts, never a new receipt."""
    required = {"stepId", "argv", "argvUtf8NulSha256", "cwd", "environmentOverrides", "ownerPID", "ownerPGID",
                "pid", "ownedPGID", "ownerOutsideChildPGID", "processStarted", "startedMonotonic", "finishedMonotonic",
                "exitCode", "signal", "resourceStop", "ownerFailure", "settlement", "allDirectChildAwaited",
                "allOwnedDescendantsConfirmedSettled", "ownedCompletionEvidence", "rawStreamsFinal", "stdoutSha256",
                "stderrSha256", "truncated", "guardReceipt", "guardReceiptSha256", "postObservationFailure",
                "postInputIdentity", "wrapperExitCode"}
    api.require(required <= set(receipt), "native shared-ops receipt missing required facts: " + ",".join(sorted(required - set(receipt))))
    _actual_binding(api, final, receipt["argv"], receipt["cwd"])
    api.require(receipt["stepId"] == final.get("label"), "native shared-ops final step identity differs")
    argv_hash = api.hashlib.sha256(b"\0".join(item.encode("utf-8") for item in receipt["argv"]) + b"\0").hexdigest()
    api.require(receipt["argvUtf8NulSha256"] == argv_hash, "native shared-ops argv bytes differ")
    api.require(isinstance(receipt["environmentOverrides"], dict) and
                all(isinstance(key, str) and isinstance(value, str) for key, value in receipt["environmentOverrides"].items()) and
                receipt["environmentOverrides"] == final.get("environment_overrides"),
                "native shared-ops environment binding missing/different")
    api.require(type(receipt["exitCode"]) is int and receipt["exitCode"] == 0 and
                type(receipt["wrapperExitCode"]) is int and receipt["wrapperExitCode"] == 0 and
                receipt["signal"] is None and receipt["resourceStop"] is None and receipt["ownerFailure"] is None and
                receipt["postObservationFailure"] is None, "native shared-ops command/wrapper failed/aborted")
    api.require(receipt["processStarted"] is True and receipt["ownerOutsideChildPGID"] is True and
                all(type(receipt[name]) is int and receipt[name] > 0 for name in ("pid", "ownedPGID", "ownerPID", "ownerPGID")) and
                receipt["pid"] == receipt["ownedPGID"] and receipt["ownerPGID"] != receipt["ownedPGID"] and receipt["ownerPID"] != receipt["pid"],
                "native shared-ops process ownership facts invalid")
    settlement = receipt["settlement"]
    api.require(isinstance(settlement, dict) and settlement.get("settled") is True and
                type(settlement.get("exitCode")) is int and settlement["exitCode"] == 0 and
                receipt["allDirectChildAwaited"] is True, "native shared-ops direct child not settled")
    events = settlement.get("events")
    api.require(isinstance(events, list) and all(isinstance(event, dict) for event in events) and any(event.get("phase") == "WAIT_NATURAL" and
                event.get("outcome") == "wait-returned" and type(event.get("exitCode")) is int and event["exitCode"] == 0 for event in events) and
                not any(event.get("phase") in ("TERM", "KILL", "WAIT_AFTER_TERM", "WAIT_AFTER_KILL") or
                        event.get("outcome") in ("error", "timeout") for event in events),
                "native shared-ops natural wait not established")
    completion = receipt["ownedCompletionEvidence"]
    api.require(receipt["allOwnedDescendantsConfirmedSettled"] is True and isinstance(completion, dict) and
                completion.get("confirmedSettled") is True and completion.get("ownedPGID") == receipt["ownedPGID"] and
                completion.get("liveMembers") == [] and completion.get("logHolders") is False and
                "observationFailure" not in completion, "native shared-ops descendants/log holders not settled")
    process_probe = completion.get("processProbe", {}); log_probe = completion.get("logProbe", {})
    api.require(isinstance(process_probe, dict) and isinstance(log_probe, dict) and
                type(process_probe.get("exitCode")) is int and process_probe["exitCode"] == 0 and
                type(process_probe.get("stderrBytes")) is int and process_probe["stderrBytes"] == 0 and
                type(log_probe.get("positiveProbeExit")) is int and log_probe["positiveProbeExit"] == 0 and
                type(log_probe.get("exitCode")) is int and log_probe["exitCode"] == 1 and
                log_probe.get("stdoutBase64") == "" and log_probe.get("stderrBase64") == "",
                "native shared-ops completion observation incomplete")
    exited = completion.get("confirmedExitedSnapshots")
    api.require(isinstance(exited, list) and all(isinstance(item, dict) and item.get("disposition") in
                ("confirmed-gone", "confirmed-same-identity-zombie") for item in exited),
                "native shared-ops exit confirmation incomplete")
    api.require(receipt["rawStreamsFinal"] is True and receipt["truncated"] is False,
                "native shared-ops raw streams incomplete/truncated")
    sizes = {}
    for stream in ("stdout", "stderr"):
        path, digest = absolute_file(api, parent, final.get(stream), inputs)
        api.require(digest == api.digest(receipt[stream + "Sha256"]) and
                    (stream + "Sha256AtReceipt" not in receipt or receipt[stream + "Sha256AtReceipt"] == digest),
                    "native shared-ops raw " + stream + " hash differs")
        sizes[stream] = path.stat().st_size
    guard_path, _ = absolute_file(api, parent, {"path": receipt["guardReceipt"], "sha256": receipt["guardReceiptSha256"]}, inputs)
    _, guard = qualification_document(api, guard_path, inputs)
    before = guard.get("source", {}); after = receipt["postInputIdentity"]
    api.require(guard.get("passed") is True and guard.get("stepId") == receipt["stepId"] and
                guard.get("argvUtf8NulSha256") == argv_hash and guard.get("clean") is False and
                isinstance(before, dict) and isinstance(after, dict) and before.get("sourceMatchesFrozen") is True and after.get("sourceMatchesFrozen") is True and
                before.get("drift") == [] and after.get("drift") == [] and
                before.get("sourceDigest") == after.get("sourceDigest") == final.get("source_digest"),
                "native shared-ops guard/source binding differs or drifted")
    api.digest(final.get("source_digest"))
    times = [receipt["startedMonotonic"], receipt["finishedMonotonic"]]
    api.require(all(type(value) in (int, float) and math.isfinite(value) and value >= 0 for value in times) and times[1] >= times[0],
                "native shared-ops monotonic clock invalid")
    # These fields are an in-memory consumer view only. No fabricated Supreme
    # file, wall clock, bytesSeen counter or original zero is written anywhere.
    result = {"label": receipt["stepId"], "command": receipt["argv"], "cwd": receipt["cwd"],
              "exitCode": receipt["exitCode"], "signal": receipt["signal"], "errorCode": receipt["ownerFailure"],
              "truncated": receipt["truncated"], "_native_receipt_kind": "shared-ops", "_native_original": receipt,
              "_native_raw_file_bytes": sizes, "_native_clock": {"kind": "monotonic", "start": times[0], "finish": times[1]}}
    if "startedAt" in receipt or "finishedAt" in receipt:
        for field in ("startedAt", "finishedAt"):
            api.require(isinstance(receipt.get(field), str), "native shared-ops explicit UTC clock incomplete")
            parsed = datetime.fromisoformat(receipt[field].replace("Z", "+00:00"))
            api.require(parsed.tzinfo is not None, "native shared-ops UTC timezone absent")
            result[field] = receipt[field]
        api.require(datetime.fromisoformat(result["finishedAt"].replace("Z", "+00:00")) >=
                    datetime.fromisoformat(result["startedAt"].replace("Z", "+00:00")), "native shared-ops UTC completion before start")
        result["_native_clock"]["kind"] = "explicit-utc-and-monotonic"
    return final, result

def success_step(api, parent, descriptor, inputs):
    """One explicitly selected original step; failed history stays attached."""
    path, _ = absolute_file(api, parent, descriptor, inputs)
    _, receipt = qualification_document(api, path, inputs)
    finals = descriptor.get("final_steps")
    api.require(isinstance(finals, list) and len(finals) == 1,
                "native build needs one explicitly bound final step")
    final = finals[0]
    api.require(isinstance(final, dict), "native final descriptor missing")
    if "steps" not in receipt and ("stepId" in receipt or "argv" in receipt):
        return _ops_step(api, parent, receipt, final, inputs)
    if "steps" not in receipt and ("exit_code" in receipt or "natural_exit" in receipt):
        return _small_window_step(api, receipt, final)
    required = {"label", "command", "cwd", "logPath", "exitCode", "signal", "errorCode",
                "truncated", "bytesSeen", "bytesWritten", "startedAt", "finishedAt"}
    if "command" in receipt:
        steps = [receipt]
    else:
        api.require(receipt.get("schemaVersion") == 1 and isinstance(receipt.get("steps"), list),
                    "native original receipt schema unsupported")
        steps = receipt["steps"]
    matches = [step for step in steps if isinstance(step, dict) and step.get("label") == final.get("label")]
    api.require(len(matches) == 1 and required <= set(matches[0]),
                "native final step missing/ambiguous/incomplete")
    step = matches[0]
    api.require(not api.schema_example(step), "native schema-only build receipt")
    api.require(step["command"] == final.get("command") and step["cwd"] == final.get("cwd"),
                "native final command/cwd mismatch")
    api.require(type(step["exitCode"]) is int and step["exitCode"] == 0 and
                step["signal"] is None and step["errorCode"] is None,
                "native final build failed/aborted")
    api.require(step["truncated"] is False and type(step["bytesSeen"]) is int and
                step["bytesSeen"] == step["bytesWritten"] and step["bytesSeen"] >= 0,
                "native final build output incomplete")
    for field in ("label", "cwd", "logPath", "startedAt", "finishedAt"):
        api.nonempty(step[field], "native build " + field)
    api.require(step["finishedAt"] >= step["startedAt"], "native build completion before start")
    # Preserve existing natural command receipt; no synthetic exit normalization.
    return final, step

def qualify_fixture(api, dataset, dataset_path, inputs):
    """Freeze/identity checks, not a second TaskRevisionService/DAG validator."""
    if dataset.get("generation", {}).get("case") == "native-chat-timeline":
        api.require(dataset.get("protocol_sha256") == CHAT_PROTOCOL_SHA and
                    api.canonical_sha256(dataset.get("protocol")) == CHAT_PROTOCOL_SHA,
                    "chat protocol is not the exact current canonical v2 source")
        api.require(isinstance(dataset.get("fixed_workspace"), str) and Path(dataset["fixed_workspace"]).is_absolute(),
                    "chat fixed workspace missing")
        records = [r for r in dataset.get("files", []) if r.get("path") == "native-chat.json"]
        api.require(len(records) == 1, "chat fixture missing/ambiguous")
        snapshot = api.referenced(dataset_path, dataset.get("snapshot"))
        path, fixture_sha = absolute_file(api, dataset_path, {"path": str(snapshot / "native-chat.json"), "sha256": records[0]["sha256"]}, inputs)
        _, fixture = qualification_document(api, path, inputs)
        api.require(fixture.get("kind") == "native-chat-timeline" and fixture.get("input_class") == "actual-backend-capture",
                    "synthetic controls cannot qualify native samples")
        api.require(fixture.get("source_sha256") == dataset.get("metadata", {}).get("source", {}).get("source_sha256"),
                    "chat capture source differs from actual build capsule")
        # Reuse the original capture and original successful step, not a label
        # supplied by a JS functional control. All ordinary capture bytes remain
        # separate original inputs; no status or final text is manufactured here.
        capture = fixture.get("capture", {})
        api.require(capture.get("stages_sha256") == api.canonical_sha256(fixture.get("stages")), "chat stage bytes lack capture binding")
        index_path, _ = absolute_file(api, dataset_path, capture["index"], inputs)
        _, index = qualification_document(api, index_path, inputs)
        api.require(index.get("producer_test") == "native_unknown_actual_backend_replay_capture", "unknown chat backend capture")
        retained = dataset["metadata"]["source"]["profiles"]["retained_binaries"]
        if "execution_kind" in capture:
            proof = qualify_original707_capture(api, dataset_path, capture, retained, Path(dataset["metadata"]["source"]["eko"]["root"]),
                {item["path"]: item["sha256"] for item in dataset["metadata"]["source"]["eko"]["files"]}, inputs)
            stdout = Path(proof["stdout"])
        else:
            final, capture_step = success_step(api, dataset_path, capture["receipt"], inputs)
            capture_command_identity(api, final.get("command"), retained)
            stdout, _ = absolute_file(api, dataset_path, capture["stdout"], inputs)
            api.require(str(stdout) == capture_step["logPath"], "chat capture stdout is not the original step stream")
        api.require(re.search(r"test result: ok\. 1 passed; 0 failed; 0 ignored; 0 measured;", stdout.read_text()) is not None,
                    "chat capture did not execute exactly the selected test")
        def original_marker(marker, descriptor):
            found = [api.decode_json(line.split(marker, 1)[1]) for line in stdout.read_text().splitlines() if marker in line]
            matches = [item for item in found if item.get("path") == descriptor["path"] and item.get("sha256") == descriptor["sha256"]]
            api.require(len(matches) == 1 and matches[0].get("bytes") == Path(descriptor["path"]).stat().st_size,
                        "capture artifact absent from original successful stdout: " + marker)
        original_marker("EKO_NATIVE_TIMELINE_CAPTURE_INDEX:", capture["index"])
        original_marker("EKO_NATIVE_TIMELINE_CAPTURE_FILE:", capture["tool_events"])
        source_files = {item["path"]: item["sha256"] for item in dataset["metadata"]["source"]["eko"]["files"]}
        compiled_paths = {"helper": "src/tauri/commands/conversations_native_history_tests.rs",
                          "capture_module": "src/tauri/commands/native_timeline_actual_backend_capture.rs",
                          "tool_capture": "src/tauri/commands/native_timeline_tool_backend_capture.rs",
                          "get_projection": "src/tauri/commands/conversations.rs",
                          "native_publication_projection": "echo-agent-app-core/src/conversation_projection.rs"}
        api.require(all(index.get("compiled_source_sha256", {}).get(key) == source_files.get(path) and source_files.get(path)
                        for key, path in compiled_paths.items()), "chat capture compiled source differs from final source")
        stages = fixture.get("stages", [])
        api.require([s.get("operation") for s in stages] == list(OPERATIONS) and
                    all(s.get("actions") and isinstance(s.get("expected"), dict) for s in stages), "chat stages incomplete")
        actions = [a for s in stages for a in s["actions"]]
        matches = [case for case in index.get("cases", []) if case.get("case_id") == capture.get("case_id")]
        api.require(len(matches) == 1, "chat capture case missing/ambiguous")
        case = matches[0]
        api.require(capture["tool_events"] == case.get("tool_events") and
                    capture.get("delivery_trace") == case.get("delivery_trace"),
                    "chat tool/trace descriptors are not the original indexed capture")
        trace_path, _ = absolute_file(api, dataset_path, capture["delivery_trace"], inputs)
        original_marker("EKO_NATIVE_TIMELINE_CAPTURE_FILE:", capture["delivery_trace"])
        validate_chat_delivery_order(api, actions, api.decode_json(trace_path.read_text()))
        event_path, _ = absolute_file(api, index_path, case["envelopes"], inputs)
        original_events = api.decode_json(event_path.read_text())
        allowed = {"assistant_message", "tool_call", "tool_result", "execution_status", "final_answer"}
        original_agent = [e for e in original_events if e.get("payload", {}).get("source") == "agent"
                          and e["payload"]["event"]["payload"].get("type") in allowed]
        api.require([a["envelope"] for a in actions if a.get("kind") == "agent"] == original_agent, "chat native Agent events changed/reordered")
        get_path, _ = absolute_file(api, index_path, case["source_get_message"], inputs)
        original_get = api.decode_json(get_path.read_text())
        api.require([a["message"] for a in actions if a.get("kind") == "publication"] == [original_get], "chat final publication is not the original get DTO")
        tools_path, _ = absolute_file(api, dataset_path, capture["tool_events"], inputs)
        api.require([a for a in actions if a.get("kind") == "tool"] == api.decode_json(tools_path.read_text()), "chat tool summaries differ from actual listener trace")
        types = [a.get("envelope", {}).get("payload", {}).get("event", {}).get("payload", {}).get("type")
                 for a in actions if a.get("kind") == "agent"]
        api.require(set(("assistant_message", "tool_call", "tool_result", "execution_status", "final_answer")) <= set(types)
                    and any(a.get("kind") == "tool" for a in actions) and any(a.get("kind") == "publication" for a in actions),
                    "chat fixture omits real consumer paths")
        return {"sha256": fixture_sha, "objects": 1, "file": "native-chat.json", "workload": "native-chat-timeline",
                "stages": {s["operation"]: s for s in stages}}
    api.require(dataset.get("protocol_sha256") == PROTOCOL_SHA and dataset.get("protocol", {}).get("warmup") == 5 and
                dataset["protocol"].get("hotSamples") == 30 and dataset["protocol"].get("processColdSamples") == 10,
                "native samples changed the frozen warm/hot/cold protocol")
    api.require(dataset.get("generation", {}).get("case") == "gui" and
                isinstance(dataset.get("fixed_workspace"), str) and Path(dataset["fixed_workspace"]).is_absolute(),
                "native dataset is not the legal GUI fixture at its fixed absolute root")
    fixture_records = [item for item in dataset.get("files", []) if item.get("path") == "gui.json"]
    api.require(len(fixture_records) == 1, "native GUI fixture file missing/ambiguous")
    snapshot = api.referenced(dataset_path, dataset.get("snapshot"))
    fixture_path, fixture_sha = absolute_file(api, dataset_path, {"path": str(snapshot / "gui.json"),
                                                            "sha256": fixture_records[0]["sha256"]}, inputs)
    _, fixture = qualification_document(api, fixture_path, inputs)
    api.require(fixture.get("qualification") ==
                "commit_task_plan + TaskRevisionService.apply_patch" and fixture.get("objects") in (1, 10, 50, 100) and
                fixture.get("graph_depth") == 1 and fixture.get("max_tasks") == 100 and fixture.get("max_depth") == 10,
                "native input is not the frozen Rust-accepted GUI graph")
    api.require(FROZEN_GUI_BYTES.get(fixture["objects"]) == fixture_sha,
                "native fixture bytes are not one of the four frozen Rust owner-qualified inputs")
    stages = {item["stage"]: item for item in fixture.get("snapshots", [])}
    api.require(set(stages) == {"initial", "single-row", "batch", "run-state"}, "native four snapshots missing")
    return {"sha256": fixture_sha, "objects": fixture["objects"], "stages": stages, "file": "gui.json"}

def capture_command_identity(api, command, retained):
    selector = "tauri::commands::conversations::native_history_tests::native_timeline_actual_backend_capture::native_unknown_actual_backend_replay_capture"
    api.require(isinstance(command, list) and len(command) >= 4 and command[1] == selector,
                "chat capture selector/executor differs from exact original producer")
    binary = command[0]
    api.require(binary in retained and Path(binary).is_file() and api.sha256_file(Path(binary)) == retained[binary],
                "chat capture executor is not the exact retained source binary")
    flags = command[2:]
    api.require(flags.count("--exact") == flags.count("--ignored") == 1 and
                set(flags) <= {"--exact", "--ignored", "--nocapture", "--test-threads=1"},
                "chat capture flags do not execute exactly the selected ignored test")

def validate_capture_members(api, index_path, index, markers):
    """原五case/41文件/40成员的一一对应；无状态或业务字段合成。"""
    root = index_path.parent.resolve(strict=True)
    api.require([case.get("case_id") for case in index.get("cases", [])] == ["normal-unknown-1", "normal-unknown-2", "guard-unknown-1", "guard-unknown-2", "tool-progress-signed"], "actual five capture cases missing/reordered")
    actual = {}
    for descriptor in markers:
        path = Path(descriptor["path"])
        canonical = path.resolve(strict=True)
        api.require(path.is_absolute() and path == canonical and not path.is_symlink() and path.is_file() and canonical.is_relative_to(root) and canonical not in actual, "capture FILE canonical identity duplicate/outside root")
        api.require(path.stat().st_size == descriptor["bytes"] and api.sha256_file(path) == descriptor["sha256"], "capture FILE bytes/SHA differ")
        actual[canonical] = descriptor
    api.require(len(actual) == 41 and index_path.resolve() in actual, "capture FILE set not41 with index")
    physical = {path.resolve() for path in root.rglob("*") if path.is_file()}
    api.require(not any(path.is_symlink() for path in root.rglob("*")) and physical == set(actual), "capture physical set differs from original FILE markers")
    expected = {path: descriptor for path, descriptor in actual.items() if path != index_path.resolve()}
    references = []
    for case in index["cases"]:
        case_root = (root / case["case_id"]).resolve(strict=True)
        members = [value for value in case.values() if isinstance(value, dict) and {"path", "sha256", "bytes"} <= value.keys()]
        api.require(len(members) == 8, "capture case must reference eight members")
        for descriptor in members:
            path = Path(descriptor["path"]).resolve(strict=True)
            api.require(path.parent == case_root and path in expected and descriptor == expected[path], "capture member belongs to wrong case/FILE")
            references.append(path)
    api.require(len(references) == len(set(references)) == 40 and set(references) == set(expected), "capture 40 canonical members are not a FILE bijection")

def validate_original707_child(api, outer_step, process, start, retained):
    """两层成功条件分别校验；可直接用于小metadata反例。"""
    api.require(type(outer_step.get("exitCode")) is int and outer_step["exitCode"] == 0 and
                type(process.get("exit")) is int and process["exit"] == 0 and process.get("args") == start.get("argv"),
                "capture child failed or args differ despite outer success")
    capture_command_identity(api, process["args"], retained)
    binary = start.get("binary", {})
    api.require(process["args"][0] == binary.get("path") and retained.get(binary.get("path")) == binary.get("sha256"), "capture child actual binary differs")

def qualify_original707_capture(api, parent, capture, retained, source_root, source_files, inputs):
    """只识别已知原707 child与外Python的两层原件，绝不改成直接receipt。"""
    api.require(capture.get("execution_kind") == "original-707-child-v1", "explicit original707 capture kind required")
    accepted_path, accepted_sha = absolute_file(api, parent, capture["accepted_capture"], inputs)
    api.require(accepted_sha == "5bcadb493bc9af470ade10f315a5a1d963cae99890483a771faaa34e19c026b1", "capture accepted authority differs")
    _, accepted = qualification_document(api, accepted_path, inputs)
    result_path, _ = absolute_file(api, parent, capture["capture_result"], inputs)
    _, original_result = qualification_document(api, result_path, inputs)
    api.require(accepted.get("state") == "ROOT_ACCEPTED_CURRENT080_F340_ALL_FOUR_ROLES_NINE_LISTS_IO2_HEADLESS5_AND_NORUN_TARGET_RELEASED" and
                accepted.get("capture", {}).get("result") == capture["capture_result"] and original_result.get("natural_exit") == 0,
                "capture original accepted result binding differs")
    for field, original in (("process", "process"), ("stdout", "stdout"), ("stderr", "stderr"), ("index", "capture_index")):
        api.require(capture.get(field) == original_result.get(original), "capture reference differs from original accepted result: " + field)
    outer, outer_step = success_step(api, parent, capture["receipt"], inputs)
    process_path, _ = absolute_file(api, parent, capture["process"], inputs)
    _, process = qualification_document(api, process_path, inputs)
    start_path, _ = absolute_file(api, parent, capture["start"], inputs)
    _, start = qualification_document(api, start_path, inputs)
    runner, runner_sha = absolute_file(api, parent, capture["runner"], inputs)
    controller, controller_sha = absolute_file(api, parent, capture["controller"], inputs)
    api.require(runner_sha == "707d628181304652f61cd623f772d8aeefa81937df2afb16d54d85b90e41f504" and
                controller_sha == "ecaae0f0a0c898f27feee14fc70cb21a8a51bbe146de41741bcd8dd8b5ec7214", "capture original runner/controller code identity differs")
    api.require(start.get("runner", {}).get("path") == str(runner) and start["runner"]["sha256"] == runner_sha, "capture start runner binding differs")
    acceptance = start.get("Root_actual_order3_acceptance", {})
    absolute_file(api, parent, acceptance, inputs)
    command = outer["command"]
    api.require(len(command) == 5 and re.fullmatch(r"python[0-9.]*", Path(command[0]).name) is not None and command[1] == "-B" and command[2] == str(controller) and
                command[3:] == [acceptance["path"], acceptance["sha256"]], "capture outer is not the original Python caller")
    api.require(start.get("override") is None and start.get("default_original_watchdog_seconds") == 300 and start.get("original_cleanup_grace_seconds") == 5,
                "capture original707 supervision differs")
    api.require(start.get("cwd") == str(source_root) and isinstance(start.get("workspace"), str) and Path(start["workspace"]).is_absolute(), "capture child start cwd/workspace differs")
    environment = start.get("environment_overlay")
    api.require(isinstance(environment, dict) and set(environment) == {"TMPDIR", "DYLD_LIBRARY_PATH", "EKO_NATIVE_TIMELINE_CAPTURE_DIR"}, "capture action/watchdog/environment override differs")
    api.require(start.get("Root_actual_order3_acceptance") == original_result.get("Root_actual_order3_acceptance") and
                start_path == process_path.parent.parent / "actual-start.json" and
                Path(capture["receipt"]["path"]) == process_path.parent.parent / "original-body-original-run.json", "capture start/outer ownership relation differs")
    validate_original707_child(api, outer_step, process, start, retained)
    stdout, stdout_sha = absolute_file(api, parent, capture["stdout"], inputs)
    stderr, stderr_sha = absolute_file(api, parent, capture["stderr"], inputs)
    api.require(process.get("stdout") == {"path": str(stdout), "sha256": stdout_sha} and process.get("stderr") == {"path": str(stderr), "sha256": stderr_sha}, "capture child streams differ")
    text = stdout.read_text()
    api.require(re.search(r"test result: ok\. 1 passed; 0 failed; 0 ignored; 0 measured;", text) is not None, "capture did not execute exact one successful test")
    index_path, _ = absolute_file(api, parent, capture["index"], inputs)
    _, index = qualification_document(api, index_path, inputs)
    api.require(environment["EKO_NATIVE_TIMELINE_CAPTURE_DIR"] == str(index_path.parent) and index_path.parent.is_relative_to(source_root.parent / ".echo-semantic/local"), "capture compiled output root differs")
    markers = [api.decode_json(line.split("EKO_NATIVE_TIMELINE_CAPTURE_FILE:", 1)[1]) for line in text.splitlines() if "EKO_NATIVE_TIMELINE_CAPTURE_FILE:" in line]
    indexes = [api.decode_json(line.split("EKO_NATIVE_TIMELINE_CAPTURE_INDEX:", 1)[1]) for line in text.splitlines() if "EKO_NATIVE_TIMELINE_CAPTURE_INDEX:" in line]
    api.require(len(indexes) == 1 and indexes[0] in markers and indexes[0]["path"] == str(index_path) and indexes[0]["sha256"] == api.sha256_file(index_path), "capture original INDEX marker differs")
    validate_capture_members(api, index_path, index, markers)
    paths = {"helper": "src/tauri/commands/conversations_native_history_tests.rs", "capture_module": "src/tauri/commands/native_timeline_actual_backend_capture.rs",
             "tool_capture": "src/tauri/commands/native_timeline_tool_backend_capture.rs", "get_projection": "src/tauri/commands/conversations.rs",
             "native_publication_projection": "echo-agent-app-core/src/conversation_projection.rs"}
    api.require(all(index.get("compiled_source_sha256", {}).get(key) == source_files.get(path) and source_files.get(path) for key, path in paths.items()), "capture five compiled source identities differ")
    api.require(index.get("producer_test") == "native_unknown_actual_backend_replay_capture", "capture producer differs")
    return {"status": "recognized_success", "index": index, "index_path": str(index_path), "stdout": str(stdout),
            "outer_cwd": outer_step["cwd"], "child_cwd_from_original_start": start["cwd"], "process_scope": process.get("scope"),
            "metric_scope": "whole child rusage; not per-API latency", "files": 41, "cases": 5}

def validate_chat_delivery_order(api, actions, original_deliveries):
    """Only selects existing callback facts; never synthesizes a tool summary."""
    allowed = {"assistant_message", "tool_call", "tool_result", "execution_status", "final_answer"}
    api.require(isinstance(original_deliveries, list) and original_deliveries, "original GUI callback trace missing")
    selected = []
    for entry in original_deliveries:
        if entry.get("kind") == "tool":
            selected.append(entry)
        elif entry.get("kind") == "agent":
            envelope = entry.get("envelope", {})
            api.require(envelope.get("payload", {}).get("source") == "agent", "GUI callback trace relabels a source")
            if envelope["payload"]["event"]["payload"].get("type") in allowed:
                selected.append(entry)
        else:
            api.require(False, "unknown GUI callback trace entry")
    api.require([entry for entry in actions if entry.get("kind") != "publication"] == selected,
                "chat stage actions changed/reordered original GUI callbacks")
