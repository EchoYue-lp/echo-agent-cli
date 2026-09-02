//! Bounded text-channel delivery and presentation policy.

use std::sync::{Mutex, MutexGuard};

use super::ChannelRenderEvent;

pub(super) const CHANNEL_EVENT_QUEUE_CAPACITY: usize = 64;
const CHANNEL_TURN_EVENT_LIMIT: usize = 2_048;
const CHANNEL_TURN_EVENT_BYTES: usize = 512 * 1024;
const CHANNEL_TURN_ENQUEUED_MESSAGES: usize = 512;
pub(super) const CHANNEL_TOKEN_COALESCE_BYTES: usize = 1024;

#[derive(Default)]
struct ChannelSinkBudget {
    accepted_events: usize,
    accepted_bytes: usize,
    enqueued_messages: usize,
    pending_token: String,
}

/// Ephemeral bounded operation boundary over the canonical journal and tool projection.
pub(super) struct ChannelSurfaceSink {
    tx: tokio::sync::mpsc::Sender<ChannelRenderEvent>,
    cancel: echo_agent::agent::CancellationToken,
    budget: Mutex<ChannelSinkBudget>,
}

impl ChannelSurfaceSink {
    pub(super) fn new(
        tx: tokio::sync::mpsc::Sender<ChannelRenderEvent>,
        cancel: echo_agent::agent::CancellationToken,
    ) -> Self {
        Self {
            tx,
            cancel,
            budget: Mutex::new(ChannelSinkBudget::default()),
        }
    }

    fn deliver_driver(
        &self,
        event: echo_agent_app_core::api::chat_driver::ChatDriverEvent,
    ) -> bool {
        let event_bytes = match channel_serialized_size(&event, CHANNEL_TURN_EVENT_BYTES) {
            Some(bytes) => bytes,
            None => return self.reject_delivery("channel event exceeds turn byte budget"),
        };
        let mut budget = lock_channel_sink_budget(&self.budget);
        if !accept_channel_event(&mut budget, event_bytes) {
            return self.reject_delivery("channel event budget exhausted");
        }
        if let echo_agent_app_core::api::chat_driver::ChatDriverEvent::Agent(envelope) = &event
            && let echo_agent::agent::AgentEvent::Token(token) = &envelope.payload
        {
            return self.append_token(&mut budget, token);
        }
        if !self.flush_pending_token(&mut budget) {
            return false;
        }
        self.try_send(&mut budget, ChannelRenderEvent::Driver(event))
    }

    fn deliver_projection(
        &self,
        update: echo_agent_app_core::api::tool_execution_projection::ToolExecutionProjectionUpdate,
    ) -> bool {
        let event_bytes = match channel_serialized_size(&update.summary, CHANNEL_TURN_EVENT_BYTES) {
            Some(bytes) => bytes.saturating_add(update.agent.len()),
            None => return self.reject_delivery("channel tool projection exceeds byte budget"),
        };
        let mut budget = lock_channel_sink_budget(&self.budget);
        if !accept_channel_event(&mut budget, event_bytes) {
            return self.reject_delivery("channel projection budget exhausted");
        }
        if !self.flush_pending_token(&mut budget) {
            return false;
        }
        self.try_send(&mut budget, ChannelRenderEvent::ToolProjection(update))
    }

    fn deliver_journaled(
        &self,
        envelope: echo_agent_app_core::api::chat_event_log::ChatEventEnvelope,
    ) -> bool {
        let event_bytes = match channel_serialized_size(&envelope, CHANNEL_TURN_EVENT_BYTES) {
            Some(bytes) => bytes,
            None => return self.reject_delivery("journaled channel event exceeds byte budget"),
        };
        let mut budget = lock_channel_sink_budget(&self.budget);
        if !accept_channel_event(&mut budget, event_bytes) {
            return self.reject_delivery("journaled channel event budget exhausted");
        }
        if let echo_agent_app_core::api::chat_driver::ChatDriverEvent::Agent(agent) =
            &envelope.payload
            && let echo_agent::agent::AgentEvent::Token(token) = &agent.payload
        {
            return self.append_token(&mut budget, token);
        }
        if !self.flush_pending_token(&mut budget) {
            return false;
        }
        self.try_send(&mut budget, ChannelRenderEvent::Journaled(envelope))
    }

    fn append_token(&self, budget: &mut ChannelSinkBudget, token: &str) -> bool {
        for character in token.chars() {
            if !budget.pending_token.is_empty()
                && budget
                    .pending_token
                    .len()
                    .saturating_add(character.len_utf8())
                    > CHANNEL_TOKEN_COALESCE_BYTES
                && !self.flush_pending_token(budget)
            {
                return false;
            }
            budget.pending_token.push(character);
        }
        if budget.pending_token.len() >= CHANNEL_TOKEN_COALESCE_BYTES {
            return self.flush_pending_token(budget);
        }
        true
    }

    fn flush_pending_token(&self, budget: &mut ChannelSinkBudget) -> bool {
        if budget.pending_token.is_empty() {
            return true;
        }
        let token = std::mem::take(&mut budget.pending_token);
        self.try_send(budget, ChannelRenderEvent::Token(token))
    }

    fn try_send(&self, budget: &mut ChannelSinkBudget, event: ChannelRenderEvent) -> bool {
        if budget.enqueued_messages >= CHANNEL_TURN_ENQUEUED_MESSAGES {
            return self.reject_delivery("channel message budget exhausted");
        }
        match self.tx.try_send(event) {
            Ok(()) => {
                budget.enqueued_messages = budget.enqueued_messages.saturating_add(1);
                true
            }
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                self.reject_delivery("channel event queue is full")
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                self.reject_delivery("channel event receiver is closed")
            }
        }
    }

    fn reject_delivery(&self, reason: &str) -> bool {
        tracing::warn!(reason, "channel surface delivery stopped");
        self.cancel.cancel();
        false
    }
}

impl echo_agent_app_core::api::chat_driver::ChatSink for ChannelSurfaceSink {
    fn on_event(&self, event: echo_agent_app_core::api::chat_driver::ChatDriverEvent) -> bool {
        self.deliver_driver(event)
    }

    fn on_journaled_event(
        &self,
        envelope: echo_agent_app_core::api::chat_event_log::ChatEventEnvelope,
    ) -> bool {
        self.deliver_journaled(envelope)
    }

    fn on_tool_execution_projection(
        &self,
        update: &echo_agent_app_core::api::tool_execution_projection::ToolExecutionProjectionUpdate,
    ) -> bool {
        self.deliver_projection(update.clone())
    }
}

fn lock_channel_sink_budget(mutex: &Mutex<ChannelSinkBudget>) -> MutexGuard<'_, ChannelSinkBudget> {
    mutex.lock().unwrap_or_else(|poisoned| {
        tracing::warn!("channel sink budget lock was poisoned; recovering state");
        poisoned.into_inner()
    })
}

fn accept_channel_event(budget: &mut ChannelSinkBudget, bytes: usize) -> bool {
    if budget.accepted_events >= CHANNEL_TURN_EVENT_LIMIT
        || bytes > CHANNEL_TURN_EVENT_BYTES.saturating_sub(budget.accepted_bytes)
    {
        return false;
    }
    budget.accepted_events = budget.accepted_events.saturating_add(1);
    budget.accepted_bytes = budget.accepted_bytes.saturating_add(bytes);
    true
}

struct ChannelCountingWriter {
    bytes: usize,
    limit: usize,
}

impl std::io::Write for ChannelCountingWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.bytes) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::FileTooLarge,
                "serialized channel event exceeds its delivery budget",
            ));
        }
        self.bytes = self.bytes.saturating_add(bytes.len());
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn channel_serialized_size(value: &impl serde::Serialize, limit: usize) -> Option<usize> {
    let mut writer = ChannelCountingWriter { bytes: 0, limit };
    serde_json::to_writer(&mut writer, value)
        .ok()
        .map(|()| writer.bytes)
}

pub(super) const CHANNEL_OUTBOUND_TOTAL_MESSAGES: usize = 256;
const CHANNEL_OUTBOUND_ORDINARY_MESSAGES: usize = 255;
const CHANNEL_OUTBOUND_TOTAL_BYTES: usize = 256 * 1024;
const CHANNEL_OUTBOUND_TERMINAL_BYTES: usize = 4 * 1024;
const CHANNEL_OUTBOUND_TEXT_CHARS: usize = 4_096;
const CHANNEL_OUTBOUND_TERMINAL_CHARS: usize = 320;

#[derive(Clone, Copy, PartialEq, Eq)]
enum ChannelOutboundKind {
    Ordinary,
    Buffered,
    Terminal,
}

pub(super) struct ChannelOutboundDraft {
    kind: ChannelOutboundKind,
    text: String,
    continuation: bool,
}

impl ChannelOutboundDraft {
    pub(super) fn ordinary(text: impl Into<String>) -> Self {
        Self {
            kind: ChannelOutboundKind::Ordinary,
            text: text.into(),
            continuation: false,
        }
    }

    pub(super) fn stream(text: impl Into<String>) -> Self {
        Self {
            kind: ChannelOutboundKind::Ordinary,
            text: text.into(),
            continuation: true,
        }
    }

    pub(super) fn terminal(text: impl Into<String>) -> Self {
        Self {
            kind: ChannelOutboundKind::Terminal,
            text: text.into(),
            continuation: false,
        }
    }
}

pub(super) fn channel_transport_chunk_bytes(channel_id: &str) -> usize {
    match channel_id.to_ascii_lowercase().as_str() {
        "qq" | "qqbot" => 1_800,
        "feishu" | "lark" => 4_000,
        _ => 2_000,
    }
}

pub(super) fn channel_safe_text(value: &str, max_chars: usize) -> String {
    let policy = echo_agent::utils::retention::ContentRetentionPolicy {
        max_string_chars: max_chars,
        max_array_items: 32,
    };
    if let Ok(mut structured) = serde_json::from_str::<serde_json::Value>(value) {
        policy.sanitize_json(&mut structured);
        if let Ok(serialized) = serde_json::to_string(&structured) {
            return policy.sanitize_text(&serialized);
        }
    }
    policy.sanitize_text(value)
}

pub(super) fn channel_outbound_chunks(
    channel_id: &str,
    draft: &ChannelOutboundDraft,
) -> Vec<String> {
    let max_chars = match draft.kind {
        ChannelOutboundKind::Ordinary => CHANNEL_OUTBOUND_TEXT_CHARS,
        ChannelOutboundKind::Buffered => CHANNEL_REDACTION_PENDING_BYTES,
        ChannelOutboundKind::Terminal => CHANNEL_OUTBOUND_TERMINAL_CHARS,
    };
    echo_agent::utils::utf8::split_utf8_chunks(
        channel_safe_text(&draft.text, max_chars),
        channel_transport_chunk_bytes(channel_id),
    )
}

const CHANNEL_REDACTION_PENDING_BYTES: usize =
    CHANNEL_OUTBOUND_TOTAL_BYTES - CHANNEL_OUTBOUND_TERMINAL_BYTES;

#[derive(Default)]
/// Holds dynamic continuation text until a semantic boundary permits one
/// canonical retention pass. This deliberately trades live sentence delivery
/// for provable cross-draft redaction. Overflow discards the entire buffered
/// projection and emits only a fixed notice; durable journals/artifacts remain
/// authoritative. Restoring live delivery requires a framework-owned streaming
/// redactor, not transport-local pattern prefixes.
pub(super) struct ChannelStreamingSanitizer {
    pending: String,
    omitted: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ChannelBufferOutcome {
    Buffered,
    Omitted,
}

impl ChannelStreamingSanitizer {
    pub(super) fn push(&mut self, text: &str) -> ChannelBufferOutcome {
        if self.omitted {
            return ChannelBufferOutcome::Omitted;
        }
        if text.len() > CHANNEL_REDACTION_PENDING_BYTES.saturating_sub(self.pending.len()) {
            self.pending.clear();
            self.omitted = true;
            return ChannelBufferOutcome::Omitted;
        }
        self.pending.push_str(text);
        ChannelBufferOutcome::Buffered
    }

    pub(super) fn finish(&mut self) -> Option<String> {
        if self.omitted {
            self.omitted = false;
            self.pending.clear();
            return Some(
                "[channel] streamed content omitted after reaching the retention limit."
                    .to_string(),
            );
        }
        if self.pending.is_empty() {
            return None;
        }
        Some(channel_safe_text(
            &std::mem::take(&mut self.pending),
            CHANNEL_REDACTION_PENDING_BYTES,
        ))
    }
}

#[derive(Clone, Copy)]
pub(super) struct ChannelRatePolicy {
    pub(super) burst: usize,
    pub(super) sustained_interval: std::time::Duration,
}

pub(super) fn channel_rate_policy(channel_id: &str) -> ChannelRatePolicy {
    match channel_id.to_ascii_lowercase().as_str() {
        "feishu" | "lark" => ChannelRatePolicy {
            burst: 8,
            sustained_interval: std::time::Duration::from_millis(125),
        },
        "qq" | "qqbot" => ChannelRatePolicy {
            burst: 4,
            sustained_interval: std::time::Duration::from_millis(250),
        },
        _ => ChannelRatePolicy {
            burst: 4,
            sustained_interval: std::time::Duration::from_millis(250),
        },
    }
}

async fn await_channel_rate(
    remaining_burst: &mut usize,
    next_sustained: &mut tokio::time::Instant,
    policy: ChannelRatePolicy,
) {
    let now = tokio::time::Instant::now();
    if let Some(deadline) = channel_rate_deadline(remaining_burst, next_sustained, now, policy)
        && now < deadline
    {
        tokio::time::sleep_until(deadline).await;
    }
}

pub(super) fn channel_rate_deadline(
    remaining_burst: &mut usize,
    next_sustained: &mut tokio::time::Instant,
    now: tokio::time::Instant,
    policy: ChannelRatePolicy,
) -> Option<tokio::time::Instant> {
    if *remaining_burst > 0 {
        *remaining_burst = remaining_burst.saturating_sub(1);
        return None;
    }
    let deadline = now.max(*next_sustained);
    *next_sustained = deadline + policy.sustained_interval;
    Some(deadline)
}

pub(super) fn channel_outbound_transport<'a>(
    drafts: futures::stream::BoxStream<'a, echo_agent::error::Result<ChannelOutboundDraft>>,
    channel_id: String,
    to: String,
    chat_type: echo_agent::channels::ChatType,
) -> futures::stream::BoxStream<'a, echo_agent::error::Result<echo_agent::channels::OutboundMessage>>
{
    let rate_policy = channel_rate_policy(&channel_id);
    channel_outbound_transport_with_rate(drafts, channel_id, to, chat_type, rate_policy)
}

#[cfg(test)]
pub(super) fn channel_outbound_transport_unpaced<'a>(
    drafts: futures::stream::BoxStream<'a, echo_agent::error::Result<ChannelOutboundDraft>>,
    channel_id: String,
    to: String,
    chat_type: echo_agent::channels::ChatType,
) -> futures::stream::BoxStream<'a, echo_agent::error::Result<echo_agent::channels::OutboundMessage>>
{
    channel_outbound_transport_with_rate(
        drafts,
        channel_id,
        to,
        chat_type,
        ChannelRatePolicy {
            burst: usize::MAX,
            sustained_interval: std::time::Duration::from_secs(1),
        },
    )
}

fn channel_outbound_transport_with_rate<'a>(
    mut drafts: futures::stream::BoxStream<'a, echo_agent::error::Result<ChannelOutboundDraft>>,
    channel_id: String,
    to: String,
    chat_type: echo_agent::channels::ChatType,
    rate_policy: ChannelRatePolicy,
) -> futures::stream::BoxStream<'a, echo_agent::error::Result<echo_agent::channels::OutboundMessage>>
{
    use echo_agent::channels::OutboundMessage;
    use futures::StreamExt;

    async_stream::try_stream! {
        let mut ordinary_messages = 0_usize;
        let mut ordinary_bytes = 0_usize;
        let mut terminal_messages = 0_usize;
        let mut terminal_bytes = 0_usize;
        let mut ordinary_limit_reported = false;
        let mut remaining_burst = rate_policy.burst;
        let mut next_sustained = tokio::time::Instant::now() + rate_policy.sustained_interval;
        let mut sanitizer = ChannelStreamingSanitizer::default();
        macro_rules! emit_sanitized {
            ($kind:expr, $text:expr) => {{
                let draft = ChannelOutboundDraft {
                    kind: $kind,
                    text: $text,
                    continuation: false,
                };
                for chunk in channel_outbound_chunks(&channel_id, &draft) {
                    if chunk.is_empty() {
                        continue;
                    }
                    let bytes = chunk.len();
                    if draft.kind == ChannelOutboundKind::Terminal {
                        if terminal_messages
                            >= CHANNEL_OUTBOUND_TOTAL_MESSAGES
                                .saturating_sub(CHANNEL_OUTBOUND_ORDINARY_MESSAGES)
                            || bytes
                                > CHANNEL_OUTBOUND_TERMINAL_BYTES.saturating_sub(terminal_bytes)
                        {
                            continue;
                        }
                        terminal_messages = terminal_messages.saturating_add(1);
                        terminal_bytes = terminal_bytes.saturating_add(bytes);
                    } else {
                        let ordinary_byte_limit = CHANNEL_OUTBOUND_TOTAL_BYTES
                            .saturating_sub(CHANNEL_OUTBOUND_TERMINAL_BYTES);
                        let content_message_limit =
                            CHANNEL_OUTBOUND_ORDINARY_MESSAGES.saturating_sub(1);
                        if ordinary_messages >= content_message_limit
                            || bytes > ordinary_byte_limit.saturating_sub(ordinary_bytes)
                        {
                            if !ordinary_limit_reported
                                && ordinary_messages < CHANNEL_OUTBOUND_ORDINARY_MESSAGES
                            {
                                let notice = "[channel] additional output was omitted; inspect the durable trace or artifact.";
                                if notice.len()
                                    <= ordinary_byte_limit.saturating_sub(ordinary_bytes)
                                {
                                    ordinary_limit_reported = true;
                                    ordinary_messages = ordinary_messages.saturating_add(1);
                                    ordinary_bytes = ordinary_bytes.saturating_add(notice.len());
                                    await_channel_rate(
                                        &mut remaining_burst,
                                        &mut next_sustained,
                                        rate_policy,
                                    )
                                    .await;
                                    yield OutboundMessage::new(
                                        &channel_id,
                                        &to,
                                        chat_type,
                                        notice,
                                    );
                                }
                            }
                            continue;
                        }
                        ordinary_messages = ordinary_messages.saturating_add(1);
                        ordinary_bytes = ordinary_bytes.saturating_add(bytes);
                        await_channel_rate(
                            &mut remaining_burst,
                            &mut next_sustained,
                            rate_policy,
                        )
                        .await;
                    }
                    yield OutboundMessage::new(&channel_id, &to, chat_type, chunk);
                }
            }};
        }
        while let Some(draft) = drafts.next().await {
            let draft = draft?;
            let input_chars = if draft.kind == ChannelOutboundKind::Terminal {
                CHANNEL_OUTBOUND_TERMINAL_CHARS
            } else {
                CHANNEL_OUTBOUND_TEXT_CHARS
            };
            if !draft.continuation {
                if let Some(tail) = sanitizer.finish() {
                    emit_sanitized!(ChannelOutboundKind::Buffered, tail);
                }
                let bounded_input = channel_safe_text(&draft.text, input_chars);
                if !bounded_input.is_empty() {
                    emit_sanitized!(draft.kind, bounded_input);
                }
                continue;
            }
            let _buffered = sanitizer.push(&draft.text);
        }
        if let Some(tail) = sanitizer.finish() {
            emit_sanitized!(ChannelOutboundKind::Buffered, tail);
        }
    }
    .boxed()
}

pub(super) fn immediate_channel_response<'a>(
    message: &echo_agent::channels::InboundMessage,
    text: impl Into<String>,
) -> futures::stream::BoxStream<'a, echo_agent::error::Result<echo_agent::channels::OutboundMessage>>
{
    use futures::StreamExt;

    let text = text.into();
    let drafts =
        futures::stream::once(async move { Ok(ChannelOutboundDraft::ordinary(text)) }).boxed();
    channel_outbound_transport(
        drafts,
        message.channel_id.clone(),
        message.reply_target().to_string(),
        message.chat_type,
    )
}

#[derive(Clone, Copy, Default)]
enum ChannelAnsiState {
    #[default]
    Text,
    Escape,
    Csi,
    Osc,
    OscEscape,
}

#[derive(Default)]
struct ChannelAnsiStripper {
    state: ChannelAnsiState,
}

impl ChannelAnsiStripper {
    fn push(&mut self, bytes: &[u8]) -> Vec<u8> {
        let mut output = Vec::with_capacity(bytes.len());
        for byte in bytes {
            self.state = match self.state {
                ChannelAnsiState::Text if *byte == 0x1b => ChannelAnsiState::Escape,
                ChannelAnsiState::Text => {
                    output.push(*byte);
                    ChannelAnsiState::Text
                }
                ChannelAnsiState::Escape if *byte == b'[' => ChannelAnsiState::Csi,
                ChannelAnsiState::Escape if *byte == b']' => ChannelAnsiState::Osc,
                ChannelAnsiState::Escape => ChannelAnsiState::Text,
                ChannelAnsiState::Csi if (0x40..=0x7e).contains(byte) => ChannelAnsiState::Text,
                ChannelAnsiState::Csi => ChannelAnsiState::Csi,
                ChannelAnsiState::Osc if *byte == 0x07 => ChannelAnsiState::Text,
                ChannelAnsiState::Osc if *byte == 0x1b => ChannelAnsiState::OscEscape,
                ChannelAnsiState::Osc => ChannelAnsiState::Osc,
                ChannelAnsiState::OscEscape if *byte == b'\\' => ChannelAnsiState::Text,
                ChannelAnsiState::OscEscape if *byte == 0x1b => ChannelAnsiState::OscEscape,
                ChannelAnsiState::OscEscape => ChannelAnsiState::Osc,
            };
        }
        output
    }
}

const CHANNEL_TERMINAL_MAX_EVENTS: usize = 512;
const CHANNEL_TERMINAL_MAX_BYTES: usize = 512 * 1024;
const CHANNEL_TERMINAL_MAX_SECONDS: u64 = 10 * 60;

pub(super) fn channel_terminal_stream(
    message: &echo_agent::channels::InboundMessage,
    initial: String,
    mut events: tokio::sync::broadcast::Receiver<echo_agent_app_core::api::terminal::TerminalEvent>,
    terminal_id: String,
) -> futures::stream::BoxStream<
    'static,
    echo_agent::error::Result<echo_agent::channels::OutboundMessage>,
> {
    use futures::StreamExt;

    let channel_id = message.channel_id.clone();
    let draft_channel_id = channel_id.clone();
    let reply_target = message.reply_target().to_string();
    let chat_type = message.chat_type;
    let drafts = async_stream::stream! {
        let mut accepted_events = 1_usize;
        let mut accepted_bytes = initial.len();
        let mut decoder = echo_agent::utils::utf8::IncrementalUtf8Decoder::new(
            channel_transport_chunk_bytes(&draft_channel_id),
        );
        let mut ansi = ChannelAnsiStripper::default();
        let deadline = tokio::time::Instant::now()
            + std::time::Duration::from_secs(CHANNEL_TERMINAL_MAX_SECONDS);
        if accepted_bytes > CHANNEL_TERMINAL_MAX_BYTES {
            yield Ok(ChannelOutboundDraft::terminal(format!(
                "Terminal '{terminal_id}' channel forwarding detached because its initial response exceeded the byte budget; use /terminal status or attach to continue."
            )));
            return;
        }
        yield Ok(ChannelOutboundDraft::ordinary(initial));
        loop {
            let received = tokio::select! {
                _ = tokio::time::sleep_until(deadline) => {
                    if let Some(text) = decoder.finish() {
                        yield Ok(ChannelOutboundDraft::stream(text));
                    }
                    yield Ok(ChannelOutboundDraft::terminal(format!(
                        "Terminal '{terminal_id}' channel forwarding detached after {CHANNEL_TERMINAL_MAX_SECONDS} seconds; use /terminal status or attach to continue."
                    )));
                    break;
                }
                received = events.recv() => received,
            };
            match received {
                Ok(echo_agent_app_core::api::terminal::TerminalEvent::Output { id, bytes })
                    if id == terminal_id =>
                {
                    accepted_events = accepted_events.saturating_add(1);
                    // PTY chunk boundaries are scheduler-dependent, so an
                    // events-per-second limit can detach a tiny fast command
                    // before its exit receipt. Total events, bytes, and wall
                    // time remain bounded independently of chunking.
                    if accepted_events > CHANNEL_TERMINAL_MAX_EVENTS
                        || bytes.len()
                            > CHANNEL_TERMINAL_MAX_BYTES.saturating_sub(accepted_bytes)
                    {
                        if let Some(text) = decoder.finish() {
                            yield Ok(ChannelOutboundDraft::stream(text));
                        }
                        yield Ok(ChannelOutboundDraft::terminal(format!(
                            "Terminal '{terminal_id}' channel forwarding detached after reaching its output budget; use /terminal status or attach to continue."
                        )));
                        break;
                    }
                    accepted_bytes = accepted_bytes.saturating_add(bytes.len());
                    let visible = ansi.push(&bytes);
                    for text in decoder.push(&visible) {
                        yield Ok(ChannelOutboundDraft::stream(text));
                    }
                }
                Ok(echo_agent_app_core::api::terminal::TerminalEvent::Exited { id, reason })
                    if id == terminal_id =>
                {
                    if let Some(text) = decoder.finish() {
                        yield Ok(ChannelOutboundDraft::stream(text));
                    }
                    yield Ok(ChannelOutboundDraft::terminal(
                        format!("Terminal '{terminal_id}' exited: {reason:?}"),
                    ));
                    break;
                }
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    if let Some(text) = decoder.finish() {
                        yield Ok(ChannelOutboundDraft::stream(text));
                    }
                    yield Ok(ChannelOutboundDraft::terminal(
                        format!(
                            "Terminal '{terminal_id}' channel forwarding detached after lagging by {skipped} event(s); use /terminal status or attach to continue."
                        ),
                    ));
                    break;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    if let Some(text) = decoder.finish() {
                        yield Ok(ChannelOutboundDraft::stream(text));
                    }
                    break;
                }
            }
        }
    }
    .boxed();
    channel_outbound_transport(drafts, channel_id, reply_target, chat_type)
}
