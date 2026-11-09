//! 显式focused捕获：原ReAct/Guard/FileStore/journal/import，不启动App或真实API。
use super::*;
use anyhow::Context as _;
use sha2::{Digest as _, Sha256};
use std::io::Write as _;
use std::path::{Path, PathBuf};

const FILE_MARKER: &str = "EKO_NATIVE_TIMELINE_CAPTURE_FILE:";
const INDEX_MARKER: &str = "EKO_NATIVE_TIMELINE_CAPTURE_INDEX:";

pub(super) fn write_original<T: serde::Serialize>(
    directory: &Path,
    name: &str,
    value: &T,
) -> anyhow::Result<serde_json::Value> {
    let path = directory.join(name);
    let bytes = serde_json::to_vec(value)?;
    // 本地仍需防止一次重跑覆盖上一份真实原件；只允许新建证据文件。
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    let receipt = serde_json::json!({
        "path": path.to_string_lossy(),
        "sha256": hex::encode(Sha256::digest(&bytes)),
        "bytes": bytes.len(),
    });
    println!("{FILE_MARKER}{}", serde_json::to_string(&receipt)?);
    Ok(receipt)
}

async fn capture_case(
    directory: &Path,
    block_count: usize,
    transform: bool,
) -> anyhow::Result<serde_json::Value> {
    let fixture = produce_and_reopen_with_unknown_blocks(transform, block_count).await?;
    let message = echo_agent::memory::restore_message(&fixture.stored)?;
    let raw = message
        .assistant_blocks
        .as_deref()
        .context("缺原raw blocks")?;
    anyhow::ensure!(
        raw.len() == block_count,
        "请求factory必须自然产生指定raw块数"
    );
    let candidate = message
        .execution_status
        .as_ref()
        .context("缺原durable Candidate")?;
    anyhow::ensure!(
        candidate.status == echo_agent::agent::ExecutionStatusKind::Candidate,
        "不能从fixture制造durable Settled"
    );
    let actual_settled = fixture
        .publications
        .iter()
        .find_map(|event| match &event.payload {
            ChatDriverEvent::Agent(agent) => match &agent.payload {
                echo_agent::agent::AgentEvent::ExecutionStatus(status)
                    if status.status == echo_agent::agent::ExecutionStatusKind::Settled =>
                {
                    Some(status)
                }
                _ => None,
            },
            _ => None,
        })
        .context("缺实际公开原Settled")?;
    anyhow::ensure!(
        (
            &candidate.execution_id,
            &candidate.request_id,
            candidate.sequence
        ) == (
            &actual_settled.execution_id,
            &actual_settled.request_id,
            actual_settled.sequence
        ),
        "原Candidate与实际Settled坐标不一致"
    );
    let approved_count = message
        .approved_assistant_blocks
        .as_deref()
        .context("缺原approved集合")?
        .len();
    anyhow::ensure!(
        approved_count == if transform { 0 } else { block_count },
        "Guard结果必须由原ReAct自然形成approved集合"
    );
    let source_get = project_stored_message_with_publications(
        fixture.stored.clone(),
        Some(WORKSPACE),
        Some(CONVERSATION),
        &fixture.publications,
    )?;
    anyhow::ensure!(
        source_get.final_answer_confirmed,
        "真实get投影尚未确认，不能捕获伪GREEN"
    );

    let source_checksum = hex::encode(Sha256::digest(serde_json::to_vec(&message)?));
    let first = fixture
        .publications
        .iter()
        .find(|event| matches!(event.payload, ChatDriverEvent::Agent(_)))
        .context("缺原Agent publication")?;
    let import = echo_agent_app_core::api::conversation_projection::NativeHistoryImport {
        source_workspace_id: first.workspace_id.clone(),
        source_conversation_id: first.conversation_id.clone().context("缺原CID")?,
        source_root_turn_id: first.root_turn_id.clone(),
        source_turn_id: first.turn_id.clone(),
        source_message_id: first.message_id.clone(),
        execution_id: candidate.execution_id.clone(),
        message_content_sha256: source_checksum.clone(),
        events: fixture.publications.clone(),
    };
    let case_id = format!(
        "{}-unknown-{block_count}",
        if transform { "guard" } else { "normal" }
    );
    let case_dir = directory.join(&case_id);
    std::fs::create_dir_all(&case_dir)?;
    let stored_receipt = write_original(&case_dir, "source-stored-message.json", &fixture.stored)?;
    let canonical_receipt = write_original(&case_dir, "source-canonical-message.json", &message)?;
    let events_receipt = write_original(&case_dir, "source-envelopes.json", &fixture.publications)?;
    let get_receipt = write_original(&case_dir, "source-get-message.json", &source_get)?;
    let import_receipt = write_original(&case_dir, "native-history-import.json", &import)?;
    let actual_terminal_receipt =
        write_original(&case_dir, "actual-terminal.json", actual_settled)?;

    let mut imported = fixture.stored.clone();
    let mut metadata: serde_json::Value = serde_json::from_str(
        imported
            .attachments_json
            .as_deref()
            .context("缺原metadata")?,
    )?;
    metadata
        .as_object_mut()
        .context("原metadata非对象")?
        .insert(
            "native_history_import".to_string(),
            serde_json::to_value(&import)?,
        );
    imported.attachments_json = Some(serde_json::to_string(&metadata)?);
    // 沿用原import测试的目标FileStore落盘/重开路径；不写新运行状态。
    let target_root = tempfile::tempdir()?;
    let target = FileConversationStore::new(target_root.path().join("transcript"))?;
    target
        .create_conversation(NewConversation {
            conversation_id: CONVERSATION.to_string(),
            user_id: "default".to_string(),
            agent_type: None,
            title: Some("Captured imported native history".to_string()),
        })
        .await?;
    target.save_messages(CONVERSATION, &[imported]).await?;
    drop(target);
    // 证据原件已同步落盘。后面的目标读取不使用source fixture/log/store。
    let source_execution = candidate.execution_id.clone();
    drop(fixture);
    let target = FileConversationStore::new(target_root.path().join("transcript"))?;
    let target_stored = target
        .get_messages(CONVERSATION)
        .await?
        .into_iter()
        .find(|row| row.role == "assistant")
        .context("target重开缺原import行")?;
    let target_canonical = echo_agent::memory::restore_message(&target_stored)?;
    anyhow::ensure!(
        hex::encode(Sha256::digest(serde_json::to_vec(&target_canonical)?)) == source_checksum,
        "import不能改写原canonical Message/checksum"
    );
    let target_get = project_stored_message_with_publications(
        target_stored.clone(),
        Some("native-timeline-import-target"),
        Some(CONVERSATION),
        &[],
    )?;
    anyhow::ensure!(target_get.final_answer_confirmed, "target冷get投影未确认");
    let target_stored_receipt =
        write_original(&case_dir, "target-stored-message.json", &target_stored)?;
    let target_get_receipt = write_original(&case_dir, "target-get-message.json", &target_get)?;
    Ok(serde_json::json!({
        "case_id": case_id, "requested_unknown_blocks": block_count, "guard_transform": transform,
        "workspace_id": WORKSPACE, "conversation_id": CONVERSATION, "root_turn_id": ROOT_TURN,
        "target_workspace_id": "native-timeline-import-target", "target_conversation_id": CONVERSATION,
        "execution_id": source_execution, "source_message_sha256": source_checksum,
        "source_stored": stored_receipt, "canonical_message": canonical_receipt,
        "envelopes": events_receipt, "source_get_message": get_receipt,
        "native_history_import": import_receipt, "actual_terminal": actual_terminal_receipt,
        "target_stored": target_stored_receipt, "target_get_message": target_get_receipt,
    }))
}

#[tokio::test]
#[ignore = "显式设置EKO_NATIVE_TIMELINE_CAPTURE_DIR并在EKO受控focused窗 --ignored --nocapture 运行；缺数据不能冒称通过"]
async fn native_unknown_actual_backend_replay_capture() -> anyhow::Result<()> {
    let main_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .context("缺主根")?
        .canonicalize()?;
    let artifact_root = main_root.join(".echo-semantic/local");
    let directory = PathBuf::from(
        std::env::var("EKO_NATIVE_TIMELINE_CAPTURE_DIR")
            .context("缺EKO_NATIVE_TIMELINE_CAPTURE_DIR，实际后端捕获待执行")?,
    );
    anyhow::ensure!(
        directory.is_absolute() && directory.starts_with(&artifact_root),
        "捕获目录必须显式指向主根ignored工程目录"
    );
    std::fs::create_dir_all(&directory)?;
    let directory = directory.canonicalize()?;
    anyhow::ensure!(
        directory.starts_with(artifact_root),
        "真实捕获只能写主根ignored工程证据目录"
    );
    let mut cases = Vec::new();
    for transform in [false, true] {
        for count in [1, 2] {
            cases.push(capture_case(&directory, count, transform).await?);
        }
    }
    cases.push(super::resource_chain_acceptance::capture_tool_case(&directory).await?);
    let index = serde_json::json!({
        "schema_version": 1,
        "producer_test": "native_unknown_actual_backend_replay_capture",
        "producer": "original ReactAgent/Guard + reopened FileConversationStore/ChatEventLog + project_stored_message_with_publications + NativeHistoryImport target cold reopen",
        "compiled_source_sha256": {
            "helper": hex::encode(Sha256::digest(include_bytes!("conversations_native_history_tests.rs"))),
            "capture_module": hex::encode(Sha256::digest(include_bytes!("native_timeline_actual_backend_capture.rs"))),
            "tool_capture": hex::encode(Sha256::digest(include_bytes!("native_timeline_tool_backend_capture.rs"))),
            "get_projection": hex::encode(Sha256::digest(include_bytes!("conversations.rs"))),
            "native_publication_projection": hex::encode(Sha256::digest(include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/echo-agent-app-core/src/conversation_projection.rs")))),
        },
        "cases": cases,
    });
    let receipt = write_original(&directory, "capture-index.json", &index)?;
    println!("{INDEX_MARKER}{}", serde_json::to_string(&receipt)?);
    Ok(())
}
