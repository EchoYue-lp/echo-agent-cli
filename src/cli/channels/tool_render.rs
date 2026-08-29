//! Ephemeral channel rendering over canonical tool projections.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use super::outbound::channel_safe_text;

pub(super) const CHANNEL_TOOL_PROGRESS_CHARS: usize = 240;
const CHANNEL_TOOL_PROGRESS_EVENTS: usize = 8;
#[cfg(test)]
pub(super) const CHANNEL_TOOL_OUTPUT_CHARS: usize = 800;
const CHANNEL_TOOL_RESULT_CHARS: usize = 500;
pub(super) const CHANNEL_ACTIVE_TOOL_LIMIT: usize = 64;
pub(super) const CHANNEL_RECENT_TOOL_TERMINALS: usize = 128;
const CHANNEL_TOOL_IDENTITY_CONFLICTS: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ChannelToolObserveOutcome {
    Accepted,
    Duplicate,
    Capacity,
    IdentityConflict,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub(super) enum ChannelToolOwner {
    Chat(String),
    Subagent(String),
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub(super) struct ChannelToolAddress {
    workspace_id: String,
    conversation_id: Option<String>,
    run_id: Option<String>,
    owner: ChannelToolOwner,
    call_id: String,
}

impl ChannelToolAddress {
    pub(super) fn chat(
        workspace_id: &str,
        conversation_id: Option<&str>,
        run_id: Option<&str>,
        message_id: &str,
        call_id: &str,
    ) -> Self {
        Self {
            workspace_id: workspace_id.to_string(),
            conversation_id: conversation_id.map(str::to_string),
            run_id: run_id.map(str::to_string),
            owner: ChannelToolOwner::Chat(message_id.to_string()),
            call_id: call_id.to_string(),
        }
    }

    pub(super) fn subagent(
        workspace_id: &str,
        conversation_id: &str,
        run_id: &str,
        subagent_run_id: &str,
        call_id: &str,
    ) -> Self {
        Self {
            workspace_id: workspace_id.to_string(),
            conversation_id: Some(conversation_id.to_string()),
            run_id: Some(run_id.to_string()),
            owner: ChannelToolOwner::Subagent(subagent_run_id.to_string()),
            call_id: call_id.to_string(),
        }
    }

    pub(super) fn from_summary(
        summary: &echo_agent_app_core::api::tool_execution::ToolExecutionSummary,
    ) -> Self {
        let owner = match &summary.owner {
            echo_agent_app_core::api::tool_execution::ToolExecutionOwner::Chat { message_id } => {
                ChannelToolOwner::Chat(message_id.clone())
            }
            echo_agent_app_core::api::tool_execution::ToolExecutionOwner::Subagent {
                subagent_run_id,
            } => ChannelToolOwner::Subagent(subagent_run_id.clone()),
        };
        Self {
            workspace_id: summary.workspace_id.clone(),
            conversation_id: summary.conversation_id.clone(),
            run_id: summary.run_id.clone(),
            owner,
            call_id: summary.call_id.clone(),
        }
    }
}

pub(super) struct ChannelToolRenderEntry {
    address: ChannelToolAddress,
    pub(super) summary: echo_agent_app_core::api::tool_execution::ToolExecutionSummary,
    output_events: usize,
    progress_events: usize,
    progress_limit_reported: bool,
}

#[derive(Default)]
pub(super) struct ChannelToolRenderState {
    pub(super) entries: HashMap<String, ChannelToolRenderEntry>,
    pub(super) addresses: HashMap<ChannelToolAddress, String>,
    pub(super) recent_terminals: HashMap<ChannelToolAddress, Option<String>>,
    canonical_addresses: HashMap<String, ChannelToolAddress>,
    recent_terminal_order: VecDeque<ChannelToolAddress>,
    identity_conflicts: HashSet<ChannelToolAddress>,
    identity_conflicts_saturated: bool,
}

impl ChannelToolRenderState {
    pub(super) fn observe(
        &mut self,
        update: echo_agent_app_core::api::tool_execution_projection::ToolExecutionProjectionUpdate,
    ) -> ChannelToolObserveOutcome {
        let address = ChannelToolAddress::from_summary(&update.summary);
        let canonical_id = update.summary.id.clone();
        if self.identity_conflicts_saturated || self.identity_conflicts.contains(&address) {
            return ChannelToolObserveOutcome::IdentityConflict;
        }
        if self
            .canonical_addresses
            .get(&canonical_id)
            .is_some_and(|existing| existing != &address)
        {
            self.quarantine_identity_conflict(address, &canonical_id);
            return ChannelToolObserveOutcome::IdentityConflict;
        }
        if let Some(recent_id) = self.recent_terminals.get(&address) {
            return if recent_id.as_deref() == Some(canonical_id.as_str()) {
                ChannelToolObserveOutcome::Duplicate
            } else {
                self.quarantine_identity_conflict(address, &canonical_id);
                ChannelToolObserveOutcome::IdentityConflict
            };
        }
        if let Some(entry) = self.entries.get(&canonical_id) {
            if entry.address != address
                || self
                    .addresses
                    .get(&address)
                    .is_some_and(|existing| existing != &canonical_id)
            {
                self.quarantine_identity_conflict(address, &canonical_id);
                return ChannelToolObserveOutcome::IdentityConflict;
            }
            let Some(entry) = self.entries.get_mut(&canonical_id) else {
                return ChannelToolObserveOutcome::IdentityConflict;
            };
            entry.summary = update.summary;
            return ChannelToolObserveOutcome::Accepted;
        }
        if self
            .addresses
            .get(&address)
            .is_some_and(|existing| existing != &canonical_id)
        {
            self.quarantine_identity_conflict(address, &canonical_id);
            return ChannelToolObserveOutcome::IdentityConflict;
        }
        if self.entries.len() >= CHANNEL_ACTIVE_TOOL_LIMIT {
            return ChannelToolObserveOutcome::Capacity;
        }
        self.addresses.insert(address.clone(), canonical_id.clone());
        self.canonical_addresses
            .insert(canonical_id.clone(), address.clone());
        self.entries.insert(
            canonical_id,
            ChannelToolRenderEntry {
                address,
                summary: update.summary,
                output_events: 0,
                progress_events: 0,
                progress_limit_reported: false,
            },
        );
        ChannelToolObserveOutcome::Accepted
    }

    fn quarantine_identity_conflict(
        &mut self,
        incoming_address: ChannelToolAddress,
        incoming_canonical_id: &str,
    ) {
        let mut affected = HashSet::from([incoming_address.clone()]);
        if let Some(address) = self.canonical_addresses.get(incoming_canonical_id) {
            affected.insert(address.clone());
        }
        if let Some(entry) = self.entries.get(incoming_canonical_id) {
            affected.insert(entry.address.clone());
        }
        if let Some(existing_id) = self.addresses.get(&incoming_address)
            && let Some(entry) = self.entries.get(existing_id)
        {
            affected.insert(entry.address.clone());
        }
        for address in &affected {
            if let Some(canonical_id) = self.addresses.remove(address) {
                self.entries.remove(&canonical_id);
            }
            self.recent_terminals.remove(address);
            self.recent_terminal_order
                .retain(|terminal| terminal != address);
        }
        self.entries.remove(incoming_canonical_id);
        self.addresses
            .retain(|_, canonical_id| canonical_id != incoming_canonical_id);
        self.canonical_addresses.retain(|canonical_id, address| {
            canonical_id != incoming_canonical_id && !affected.contains(address)
        });
        for address in affected {
            if self.identity_conflicts.len() >= CHANNEL_TOOL_IDENTITY_CONFLICTS {
                self.identity_conflicts_saturated = true;
                break;
            }
            self.identity_conflicts.insert(address);
        }
    }

    pub(super) fn entry(&self, address: &ChannelToolAddress) -> Option<&ChannelToolRenderEntry> {
        if self.identity_conflicts_saturated || self.identity_conflicts.contains(address) {
            return None;
        }
        self.addresses
            .get(address)
            .and_then(|canonical_id| self.entries.get(canonical_id))
    }

    pub(super) fn chat_address(
        &self,
        call_id: &str,
        fallback_message_id: &str,
    ) -> ChannelToolAddress {
        let active = unique_chat_address(self.addresses.keys(), call_id);
        if let Some(address) = active {
            return address;
        }
        if let Some(conflict) = self.identity_conflicts.iter().find(|address| {
            address.call_id == call_id
                && matches!(
                    &address.owner,
                    ChannelToolOwner::Chat(message_id) if message_id == fallback_message_id
                )
        }) {
            return conflict.clone();
        }
        let recent = unique_chat_address(self.recent_terminals.keys(), call_id);
        recent.unwrap_or_else(|| {
            ChannelToolAddress::chat("", None, None, fallback_message_id, call_id)
        })
    }

    fn entry_mut(&mut self, address: &ChannelToolAddress) -> Option<&mut ChannelToolRenderEntry> {
        if self.identity_conflicts_saturated || self.identity_conflicts.contains(address) {
            return None;
        }
        let canonical_id = self.addresses.get(address)?.clone();
        self.entries.get_mut(&canonical_id)
    }

    pub(super) fn detail_ref(&self, address: &ChannelToolAddress) -> Option<&str> {
        self.entry(address)
            .map(|entry| entry.summary.detail_ref.as_str())
    }

    pub(super) fn finish(&mut self, address: &ChannelToolAddress) -> ChannelToolTerminal {
        if self.recent_terminals.contains_key(address) {
            return ChannelToolTerminal::Duplicate;
        }
        if self.identity_conflicts_saturated || self.identity_conflicts.contains(address) {
            if let Some(canonical_id) = self.addresses.remove(address) {
                self.entries.remove(&canonical_id);
            }
            self.remember_terminal(address.clone(), None);
            return ChannelToolTerminal::IdentityConflict;
        }
        let canonical_id = self.addresses.remove(address);
        let entry = canonical_id
            .as_ref()
            .and_then(|canonical_id| self.entries.remove(canonical_id));
        self.remember_terminal(address.clone(), canonical_id);
        ChannelToolTerminal::Render(entry.map(Box::new))
    }

    pub(super) fn finish_owner(&mut self, owner: &ChannelToolOwner) {
        let addresses = self
            .addresses
            .keys()
            .filter(|address| &address.owner == owner)
            .cloned()
            .collect::<Vec<_>>();
        for address in addresses {
            let _terminal = self.finish(&address);
        }
    }

    fn remember_terminal(&mut self, address: ChannelToolAddress, canonical_id: Option<String>) {
        if let Some(canonical_id) = canonical_id.as_ref() {
            self.canonical_addresses
                .insert(canonical_id.clone(), address.clone());
        }
        if self
            .recent_terminals
            .insert(address.clone(), canonical_id)
            .is_none()
        {
            self.recent_terminal_order.push_back(address);
        }
        while self.recent_terminal_order.len() > CHANNEL_RECENT_TOOL_TERMINALS {
            if let Some(expired) = self.recent_terminal_order.pop_front() {
                let expired_canonical_id = self.recent_terminals.remove(&expired).flatten();
                if let Some(canonical_id) = expired_canonical_id
                    && self
                        .canonical_addresses
                        .get(&canonical_id)
                        .is_some_and(|address| address == &expired)
                {
                    self.canonical_addresses.remove(&canonical_id);
                }
            }
        }
    }

    pub(super) fn progress_preview(
        &mut self,
        address: &ChannelToolAddress,
        _message: &str,
    ) -> Option<String> {
        let Some(entry) = self.entry_mut(address) else {
            return Some("progress update available in the durable trace".to_string());
        };
        if entry.progress_events < CHANNEL_TOOL_PROGRESS_EVENTS {
            entry.progress_events = entry.progress_events.saturating_add(1);
            return Some(format!(
                "progress available in detail {}",
                channel_safe_text(&entry.summary.detail_ref, CHANNEL_TOOL_PROGRESS_CHARS)
            ));
        }
        if entry.progress_limit_reported {
            return None;
        }
        entry.progress_limit_reported = true;
        Some(format!(
            "additional progress is available in detail {}",
            channel_safe_text(&entry.summary.detail_ref, CHANNEL_TOOL_PROGRESS_CHARS)
        ))
    }

    pub(super) fn output_preview(
        &mut self,
        address: &ChannelToolAddress,
        _chunk: &str,
    ) -> Option<String> {
        let Some(entry) = self.entry_mut(address) else {
            return Some("output update available in the durable trace".to_string());
        };
        if entry.output_events >= CHANNEL_TOOL_PROGRESS_EVENTS {
            return None;
        }
        entry.output_events = entry.output_events.saturating_add(1);
        Some(format!(
            "output available in detail {}",
            channel_safe_text(&entry.summary.detail_ref, CHANNEL_TOOL_PROGRESS_CHARS)
        ))
    }
}

fn unique_chat_address<'a>(
    addresses: impl Iterator<Item = &'a ChannelToolAddress>,
    call_id: &str,
) -> Option<ChannelToolAddress> {
    let mut matches = addresses.filter(|address| {
        address.call_id == call_id && matches!(address.owner, ChannelToolOwner::Chat(_))
    });
    let first = matches.next().cloned();
    match (first, matches.next()) {
        (Some(first), None) => Some(first),
        _ => None,
    }
}

pub(super) enum ChannelToolTerminal {
    Render(Option<Box<ChannelToolRenderEntry>>),
    Duplicate,
    IdentityConflict,
}

pub(super) fn channel_tool_args_preview(args: &serde_json::Value) -> String {
    let mut args = args.clone();
    echo_agent::utils::retention::ContentRetentionPolicy {
        max_string_chars: echo_agent_app_core::api::tool_execution::TOOL_ARGS_PREVIEW_CHARS,
        max_array_items: 32,
    }
    .sanitize_json(&mut args);
    echo_agent_app_core::api::tool_execution::preview_args(&args)
}

pub(super) fn channel_tool_result_message(
    entry: Option<&ChannelToolRenderEntry>,
    artifact: Option<&echo_agent::tools::artifact::ToolOutputArtifactRef>,
    call_id: &str,
    name: &str,
    result: &echo_agent::tools::ToolResult,
) -> String {
    let raw = if result.success {
        result.output.as_str()
    } else {
        result.error.as_deref().unwrap_or(result.output.as_str())
    };
    let preview = channel_safe_text(raw, CHANNEL_TOOL_RESULT_CHARS);
    let reference = channel_tool_reference(entry.map(|entry| &entry.summary), artifact, result);
    let reference = if reference.is_empty() {
        String::new()
    } else {
        format!(" [{reference}]")
    };
    if result.success {
        format!(
            "[tool:{}] result {}: {}{}",
            channel_safe_text(call_id, CHANNEL_TOOL_PROGRESS_CHARS),
            channel_safe_text(name, CHANNEL_TOOL_PROGRESS_CHARS),
            preview,
            reference
        )
    } else {
        let failure = result.failure.as_ref().map_or_else(
            || "failed".to_string(),
            |failure| {
                format!(
                    "{} -> {}",
                    failure.category.as_str(),
                    failure.recovery.as_str()
                )
            },
        );
        format!(
            "[tool:{}] error {} [{}]: {}{}",
            channel_safe_text(call_id, CHANNEL_TOOL_PROGRESS_CHARS),
            channel_safe_text(name, CHANNEL_TOOL_PROGRESS_CHARS),
            failure,
            preview,
            reference
        )
    }
}

fn channel_tool_reference(
    summary: Option<&echo_agent_app_core::api::tool_execution::ToolExecutionSummary>,
    artifact: Option<&echo_agent::tools::artifact::ToolOutputArtifactRef>,
    result: &echo_agent::tools::ToolResult,
) -> String {
    let artifact = artifact.map(|artifact| {
        format!(
            "artifact {} ({} bytes, sha256 {}, retention {})",
            channel_safe_text(&artifact.path.to_string_lossy(), 4_096),
            artifact.artifact_bytes,
            channel_safe_text(&artifact.sha256, CHANNEL_TOOL_PROGRESS_CHARS),
            channel_safe_text(&artifact.retention, CHANNEL_TOOL_PROGRESS_CHARS)
        )
    });
    let truncated = (result.truncated
        || result
            .metadata
            .get("output_truncated")
            .is_some_and(|value| value == "true"))
    .then(|| "truncated".to_string());
    let detail = summary.map(|summary| {
        format!(
            "detail {}",
            channel_safe_text(&summary.detail_ref, CHANNEL_TOOL_PROGRESS_CHARS)
        )
    });
    [truncated, artifact, detail]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("; ")
}

pub(super) async fn channel_verified_artifact(
    repository: Arc<echo_agent_app_core::api::tool_execution::ToolExecutionRepository>,
    summary: Option<&echo_agent_app_core::api::tool_execution::ToolExecutionSummary>,
    result: &echo_agent::tools::ToolResult,
) -> Option<echo_agent::tools::artifact::ToolOutputArtifactRef> {
    let expected = result.artifact.clone()?;
    let summary = summary?;
    let workspace_id = summary.workspace_id.clone();
    let detail_ref = summary.detail_ref.clone();
    match tokio::task::spawn_blocking(move || {
        repository.verified_artifact_reference(&workspace_id, &detail_ref)
    })
    .await
    {
        Ok(Ok(Some(artifact))) if artifact == expected => Some(artifact),
        Ok(Ok(Some(_))) => {
            tracing::warn!(
                "channel omitted a tool artifact that did not match the canonical result"
            );
            None
        }
        Ok(Ok(None)) => None,
        Ok(Err(error)) => {
            tracing::warn!(%error, "channel omitted an invalid tool artifact reference");
            None
        }
        Err(error) => {
            tracing::warn!(%error, "channel artifact validation task did not complete");
            None
        }
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ChannelExecutionToolStarted {
    pub(super) call_id: String,
    pub(super) invocation: echo_agent::agent::ToolInvocation,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ChannelExecutionToolOutput {
    pub(super) call_id: String,
    pub(super) name: String,
    pub(super) channel: Option<String>,
    pub(super) chunk: Option<String>,
    pub(super) message: Option<String>,
    pub(super) percent: Option<u8>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ChannelExecutionToolCompleted {
    pub(super) call_id: String,
    pub(super) name: String,
    pub(super) result: echo_agent::tools::ToolResult,
}
