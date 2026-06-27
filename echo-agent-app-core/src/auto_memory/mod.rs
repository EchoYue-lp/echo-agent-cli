//! Auto-memory product integration.
//!
//! Generic observation extraction and typed-memory writes live in
//! `echo_agent::evolution::auto_memory`. This module keeps only the app-side
//! project memory file integration used by CLI/TUI/GUI.

pub use echo_agent::evolution::auto_memory::{
    AutoMemoryConfig, Observation, ObservationCategory, extract_observations,
    format_observations_for_memory,
};
use std::sync::Arc;

/// Append auto-extracted observations to the project memory file.
///
/// This is an application concern: the framework writes durable typed memory,
/// while the product also maintains `.echo-agent/project.md` as prompt context.
pub fn append_to_project_memory(observations: &[Observation]) -> Result<(), String> {
    if observations.is_empty() {
        return Ok(());
    }

    let formatted = format_observations_for_memory(observations);
    let cwd = std::env::current_dir().map_err(|e| format!("Failed to get cwd: {e}"))?;
    let root = crate::utils::find_project_root(&cwd).unwrap_or(cwd);
    let memory_path = root.join(".echo-agent").join("project.md");

    if let Some(parent) = memory_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("Failed to create directory: {e}"))?;
    }

    let existing = std::fs::read_to_string(&memory_path).unwrap_or_default();
    let new_content = if let Some(marker_pos) = existing.find("## Auto-extracted observations") {
        let before = &existing[..marker_pos];
        format!("{}{}", before.trim_end(), formatted)
    } else if existing.is_empty() {
        formatted
    } else {
        format!("{}\n{}", existing.trim_end(), formatted)
    };

    std::fs::write(&memory_path, new_content)
        .map_err(|e| format!("Failed to write project memory: {e}"))?;
    Ok(())
}

/// Run auto-memory extraction and persist only the app project-memory file.
///
/// Callers that also need runtime recall should additionally call
/// `write_observations_to_memory_layer` with the runtime layer manager.
pub fn run_auto_memory_extraction(
    messages: &[(String, String)],
    config: &AutoMemoryConfig,
) -> Result<usize, String> {
    let observations = extract_observations(messages, config);
    let count = observations.len();
    if count > 0 {
        append_to_project_memory(&observations)?;
    }
    Ok(count)
}

/// App-side bridge: write observations through the agent's **shared** layer
/// manager (stage4 F1, 割裂点 6).
///
/// Previously this built a fresh per-call `MemoryLayerManager` (via
/// `ReviewIntegration::create_layer_manager`) that bypassed the agent's shared
/// security guard, audit log, and write counter/observer. Callers now pass the
/// agent's shared `MemoryLayerManager` (obtained via
/// `ReactAgent::memory_layer_manager()`), so session-end auto-memory writes
/// land on the same chokepoint as explicit `write_memory` calls.
pub async fn write_observations_to_memory_layer(
    observations: &[Observation],
    layer_manager: &Arc<echo_agent::evolution::MemoryLayerManager>,
) -> Result<usize, String> {
    echo_agent::evolution::auto_memory::write_observations_to_memory_layer(
        observations,
        layer_manager,
    )
    .await
}
