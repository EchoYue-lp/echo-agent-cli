//! Test-only contract for EKO product-surface parity.

use chrono::Utc;
use echo_agent::agent::{AgentEvent, EventEnvelope, EventIdentity};

use crate::chat_driver::ChatDriverEvent;
use crate::tasks::task_runtime::executor::ExecEvent;
use crate::tasks::task_runtime::{RuntimeEventKind, RuntimeTaskEvent};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProductSurface {
    Gui,
    Tui,
    Cli,
    Channel,
    Cron,
}

impl ProductSurface {
    const ALL: [Self; 5] = [Self::Gui, Self::Tui, Self::Cli, Self::Channel, Self::Cron];
}

struct CapabilityRow {
    capability: &'static str,
    evidence: [&'static str; 5],
}

const CAPABILITY_MATRIX: &[CapabilityRow] = &[
    CapabilityRow {
        capability: "chat_task_auto",
        evidence: [
            "ChatInput mode selector",
            "/mode chat|task|auto",
            "/mode chat|task|auto",
            "/mode chat|task|auto",
            "N/A: scheduled Task trigger has no interactive selector",
        ],
    },
    CapabilityRow {
        capability: "plan_lifecycle",
        evidence: [
            "TaskRuntime plan controls",
            "plan and task slash commands",
            "task runtime slash commands",
            "shared plan/task tools",
            "scheduled TaskRuntime plan",
        ],
    },
    CapabilityRow {
        capability: "foreground_background_cron",
        evidence: [
            "task and scheduler panels",
            "task and cron commands",
            "task and cron commands",
            "shared background tools",
            "SchedulerRunner to launch_cron_run",
        ],
    },
    CapabilityRow {
        capability: "subagent_team_result",
        evidence: [
            "execution event projection",
            "subagent runtime view",
            "structured terminal result",
            "driver execution events",
            "durable structured run result",
        ],
    },
    CapabilityRow {
        capability: "hitl_approval_input_selection",
        evidence: [
            "approval/input/selection dialogs",
            "pending HumanLoop card",
            "REPL HumanLoop provider",
            "next-message HumanLoop provider",
            "N/A: unattended run persists interaction-required failure",
        ],
    },
    CapabilityRow {
        capability: "memory",
        evidence: [
            "memory commands and panels",
            "/remember /forget /memory",
            "/remember /forget /memory",
            "layered memory tools",
            "pooled agent layered memory tools",
        ],
    },
    CapabilityRow {
        capability: "skills_mcp_browser",
        evidence: [
            "settings and tool projection",
            "skills/MCP/Browser commands and tools",
            "skills/MCP/Browser commands and tools",
            "pooled agent tool surface",
            "isolated pooled agent tool surface",
        ],
    },
    CapabilityRow {
        capability: "attachments_multimodal",
        evidence: [
            "attachment picker",
            "/attach",
            "/attach",
            "channel attachment adapter",
            "N/A: scheduled definitions have no interactive attachment source",
        ],
    },
    CapabilityRow {
        capability: "tool_stream_failure_retry_cancel",
        evidence: [
            "tool cards",
            "tool execution messages",
            "terminal tool renderer",
            "driver event renderer",
            "durable tool/run events",
        ],
    },
    CapabilityRow {
        capability: "artifact",
        evidence: [
            "open artifact action",
            "/open-artifact",
            "artifact path and metadata",
            "artifact reference text",
            "durable artifact reference",
        ],
    },
    CapabilityRow {
        capability: "usage_cache_protected_context",
        evidence: [
            "durable run inspector",
            "/trace",
            "/trace",
            "/trace [run-id]",
            "durable run trace",
        ],
    },
];

#[test]
fn capability_matrix_has_evidence_for_every_surface() {
    assert_eq!(ProductSurface::ALL.len(), 5);
    assert_eq!(CAPABILITY_MATRIX.len(), 11);
    for row in CAPABILITY_MATRIX {
        assert!(!row.capability.trim().is_empty());
        assert_eq!(row.evidence.len(), ProductSurface::ALL.len());
        assert!(row.evidence.iter().all(|value| !value.trim().is_empty()));
    }
}

#[test]
fn shared_driver_wire_contract_preserves_product_facts() -> Result<(), String> {
    let identity = EventIdentity {
        conversation_id: Some("conversation-1".to_string()),
        run_id: Some("run-1".to_string()),
        turn_id: "turn-1".to_string(),
        execution_id: Some("execution-1".to_string()),
        parent_event_id: Some("parent-1".to_string()),
    };
    let events = [
        ChatDriverEvent::Agent(Box::new(EventEnvelope::new(
            &identity,
            7,
            identity.parent_event_id.clone(),
            AgentEvent::ToolError {
                call_id: "call-1".to_string(),
                name: "shell".to_string(),
                error: "timed out".to_string(),
                failure: echo_agent::tools::ToolFailure::new(
                    echo_agent::tools::ToolFailureCategory::Timeout,
                ),
            },
        ))),
        ChatDriverEvent::Execution(ExecEvent::subagent(
            "run-1",
            "task-1",
            "execution-1",
            RuntimeEventKind::ArtifactProduced,
            serde_json::json!({
                "execution_id": "execution-1",
                "artifact": {
                    "path": "/tmp/full.log",
                    "bytes": 1234,
                    "sha256": "abc123",
                    "retention": "run",
                    "available": true
                }
            }),
        )),
        ChatDriverEvent::TurnStatus {
            status: "completed".to_string(),
        },
        ChatDriverEvent::ExecutionPath {
            requested_mode: "auto".to_string(),
            observed_path: "task_runtime".to_string(),
        },
        ChatDriverEvent::Interrupt {
            run_id: "run-1".to_string(),
            goal: "revise".to_string(),
            new_message: "use the file store".to_string(),
        },
    ];
    let values = events
        .iter()
        .map(serde_json::to_value)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    let sources = values
        .iter()
        .filter_map(|value| value.get("source"))
        .filter_map(serde_json::Value::as_str)
        .collect::<Vec<_>>();
    assert_eq!(
        sources,
        [
            "agent",
            "execution",
            "turn_status",
            "execution_path",
            "interrupt"
        ]
    );

    let agent = values
        .first()
        .ok_or_else(|| "agent event missing".to_string())?;
    assert_eq!(
        agent
            .pointer("/event/run_id")
            .and_then(serde_json::Value::as_str),
        Some("run-1")
    );
    assert_eq!(
        agent
            .pointer("/event/turn_id")
            .and_then(serde_json::Value::as_str),
        Some("turn-1")
    );
    assert_eq!(
        agent
            .pointer("/event/execution_id")
            .and_then(serde_json::Value::as_str),
        Some("execution-1")
    );
    assert_eq!(
        agent
            .pointer("/event/parent_event_id")
            .and_then(serde_json::Value::as_str),
        Some("parent-1")
    );
    assert_eq!(
        agent
            .pointer("/event/payload/type")
            .and_then(serde_json::Value::as_str),
        Some("tool_error")
    );
    assert_eq!(
        agent
            .pointer("/event/payload/data/error")
            .and_then(serde_json::Value::as_str),
        Some("timed out")
    );

    let execution = values
        .get(1)
        .ok_or_else(|| "execution event missing".to_string())?;
    assert_eq!(
        execution
            .pointer("/event/run_id")
            .and_then(serde_json::Value::as_str),
        Some("run-1")
    );
    assert_eq!(
        execution
            .pointer("/event/task_id")
            .and_then(serde_json::Value::as_str),
        Some("task-1")
    );
    assert_eq!(
        execution
            .pointer("/event/payload/artifact/path")
            .and_then(serde_json::Value::as_str),
        Some("/tmp/full.log")
    );
    assert_eq!(
        execution
            .pointer("/event/payload/artifact/sha256")
            .and_then(serde_json::Value::as_str),
        Some("abc123")
    );
    Ok(())
}

#[test]
fn cron_runtime_wire_contract_preserves_terminal_facts() -> Result<(), String> {
    let event = RuntimeTaskEvent {
        seq: 9,
        run_id: "cron-run-1".to_string(),
        task_id: Some("task-1".to_string()),
        step_id: Some("execution-1".to_string()),
        event_type: RuntimeEventKind::TaskFailed,
        payload: serde_json::json!({
            "conversation_id": "cron:daily-summary",
            "category": "provider",
            "error": "stream setup failed",
            "recovery": "retry",
            "artifact": {
                "path": "/tmp/cron.log",
                "sha256": "def456",
                "available": true
            }
        }),
        timestamp: Utc::now(),
    };
    let value = serde_json::to_value(event).map_err(|error| error.to_string())?;
    assert_eq!(
        value.get("seq").and_then(serde_json::Value::as_str),
        Some("9")
    );
    assert_eq!(
        value.get("run_id").and_then(serde_json::Value::as_str),
        Some("cron-run-1")
    );
    assert_eq!(
        value.get("task_id").and_then(serde_json::Value::as_str),
        Some("task-1")
    );
    assert_eq!(
        value.get("step_id").and_then(serde_json::Value::as_str),
        Some("execution-1")
    );
    assert_eq!(
        value.get("event_type").and_then(serde_json::Value::as_str),
        Some("task_failed")
    );
    assert_eq!(
        value
            .pointer("/payload/error")
            .and_then(serde_json::Value::as_str),
        Some("stream setup failed")
    );
    assert_eq!(
        value
            .pointer("/payload/artifact/path")
            .and_then(serde_json::Value::as_str),
        Some("/tmp/cron.log")
    );
    assert_eq!(
        value
            .pointer("/payload/artifact/sha256")
            .and_then(serde_json::Value::as_str),
        Some("def456")
    );
    Ok(())
}
