//! Training data export — converts agent runs to fine-tuning formats.
//!
//! Supports three export formats:
//! - `json`: Full Run as JSON (default)
//! - `sft`: ChatML format for supervised fine-tuning
//! - `trace`: JSONL trace events for behavior cloning

use echo_agent::trace::{Run, RunEvent};
use serde_json::Value;

/// Export a single run in the specified format.
pub fn export_run(run: &Run, format: &str) -> String {
    match format {
        "sft" => export_sft(run),
        "trace" => export_trace(run),
        _ => export_json(run),
    }
}

/// Export as JSON (full Run struct).
fn export_json(run: &Run) -> String {
    serde_json::to_string_pretty(run).unwrap_or_default()
}

/// Export as ChatML (Supervised Fine-Tuning format).
///
/// Produces JSONL with one entry per conversation turn:
/// ```json
/// {"messages": [{"role": "user", "content": "..."}, {"role": "assistant", "content": "..."}]}
/// ```
fn export_sft(run: &Run) -> String {
    let mut lines = Vec::new();

    // Extract user message from run input
    let user_msg = Value::String(run.input.clone());
    let assistant_msg = Value::String(run.final_output.clone().unwrap_or_default());

    let chatml = serde_json::json!({
        "messages": [
            {"role": "user", "content": user_msg},
            {"role": "assistant", "content": assistant_msg}
        ]
    });

    lines.push(serde_json::to_string(&chatml).unwrap_or_default());

    // Also extract tool-call patterns as separate training examples
    let mut current_tool_seq: Vec<String> = Vec::new();
    for event in &run.events {
        match event {
            RunEvent::ToolCall { name, .. } => {
                current_tool_seq.push(format!("→ {name}"));
            }
            RunEvent::ToolResult { name, success, .. } => {
                let status = if *success { "OK" } else { "FAIL" };
                current_tool_seq.push(format!("  {name}: {status}"));
                // Emit a training example for this tool-use pattern
                if !current_tool_seq.is_empty() {
                    let tool_msg = serde_json::json!({
                        "messages": [
                            {"role": "system", "content": "Tool execution trace"},
                            {"role": "user", "content": format!("Task: {}", run.input)},
                            {"role": "assistant", "content": current_tool_seq.join("\n")}
                        ]
                    });
                    lines.push(serde_json::to_string(&tool_msg).unwrap_or_default());
                    current_tool_seq.clear();
                }
            }
            _ => {}
        }
    }

    lines.join("\n")
}

/// Export as JSONL trace events (for behavior cloning / replay).
fn export_trace(run: &Run) -> String {
    let mut lines = Vec::new();

    // User input event
    lines.push(
        serde_json::to_string(&serde_json::json!({
            "type": "user_input",
            "content": run.input,
            "run_id": run.run_id
        }))
        .unwrap_or_default(),
    );

    // Tool events
    for event in &run.events {
        let line = match event {
            RunEvent::ToolCall { call_id, name, args, .. } => {
                serde_json::json!({
                    "type": "tool_call",
                    "call_id": call_id,
                    "name": name,
                    "args": args
                })
            }
            RunEvent::ToolResult { call_id, name, success, output_preview, .. } => {
                serde_json::json!({
                    "type": "tool_result",
                    "call_id": call_id,
                    "name": name,
                    "success": success,
                    "output_preview": output_preview
                })
            }
            RunEvent::ToolError { call_id, name, message } => {
                serde_json::json!({
                    "type": "tool_error",
                    "call_id": call_id,
                    "name": name,
                    "message": message
                })
            }
            _ => continue,
        };
        lines.push(serde_json::to_string(&line).unwrap_or_default());
    }

    // Final answer event
    if let Some(ref output) = run.final_output {
        lines.push(
            serde_json::to_string(&serde_json::json!({
                "type": "final_answer",
                "content": output,
                "run_id": run.run_id
            }))
            .unwrap_or_default(),
        );
    }

    lines.join("\n")
}
