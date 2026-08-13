//! 应用层 IM channel 消息处理器 —— 把 IM 消息桥接到 `AgentPool`。
//!
//! 与框架层 `AgentChannelHandler::standard`（裸建 `ReactAgent`，不经 bootstrap）不同，
//! 此处 agent 从 `AgentPool::acquire` 取，经 `AgentRuntime::bootstrap` 全套接通
//! （state_store / store / compressor / MemoryLayerManager / permission_service /
//! cache_user_id / conversation_id）。会话按平台 conversation 隔离，群聊不会按 sender
//! 交叉复用上下文。
//!
//! 归属（spec §D1-6）：`AgentPool` 是 EKO 产品概念，handler 放应用层（bin crate），
//! 不进框架 `channels.rs`。框架 `AgentChannelHandler::new` 保留供纯框架/测试用。

#[cfg(feature = "channels")]
use std::sync::Arc;

#[cfg(feature = "channels")]
use echo_agent_app_core::agent_pool::AgentPool;

#[cfg(feature = "channels")]
use echo_agent_app_core::hitl::{ChannelHumanLoopProvider, ChannelHumanLoopResolution};

#[cfg(feature = "channels")]
enum ChannelRenderEvent {
    Driver(echo_agent_app_core::chat_driver::ChatDriverEvent),
    Prompt(String),
}

/// IM channel 消息处理器：持 `AgentPool`，每 `handle` 从 pool 取/复用 per-sender agent。
///
/// TUI/GUI functional parity (AGENTS.md): channels drive chat through the
/// shared `drive_chat` entry. Holds the per-sender `AgentPool` + the
/// `TaskRuntimeStore` (so `create_complex_task` can build `ChatResources`).
/// Whether a complex run is warranted is decided by the agent itself, not
/// pre-judged here.
#[cfg(feature = "channels")]
pub struct AppChannelMessageHandler {
    pool: Arc<AgentPool>,
    store: Option<Arc<echo_agent_app_core::tasks::task_runtime::TaskRuntimeStore>>,
    review_integration: Option<Arc<echo_agent_app_core::evolution::ReviewIntegration>>,
    webhook_emitter: Arc<echo_agent_app_core::webhook::WebhookEmitter>,
    hitl: Arc<ChannelHumanLoopProvider>,
    interaction_mode:
        tokio::sync::RwLock<echo_agent_app_core::tasks::task_runtime::InteractionMode>,
}

#[cfg(feature = "channels")]
impl AppChannelMessageHandler {
    pub fn new(
        pool: Arc<AgentPool>,
        store: Option<Arc<echo_agent_app_core::tasks::task_runtime::TaskRuntimeStore>>,
        review_integration: Option<Arc<echo_agent_app_core::evolution::ReviewIntegration>>,
        webhook_emitter: Arc<echo_agent_app_core::webhook::WebhookEmitter>,
    ) -> Self {
        Self {
            pool,
            store,
            review_integration,
            webhook_emitter,
            hitl: Arc::new(ChannelHumanLoopProvider::new()),
            interaction_mode: tokio::sync::RwLock::new(
                echo_agent_app_core::tasks::task_runtime::InteractionMode::Auto,
            ),
        }
    }

    /// Per-conversation pool key.
    fn conversation_id(channel_id: &str, chat_id: &str) -> String {
        format!("channel:{channel_id}:{chat_id}")
    }

    /// Per-conversation provider cache identity.
    fn cache_user_id(channel_id: &str, chat_id: &str) -> String {
        sanitize_cache_user_id(&format!("im-{channel_id}-{chat_id}"))
    }
}

#[cfg(feature = "channels")]
#[async_trait::async_trait]
impl echo_agent::channels::MessageHandler for AppChannelMessageHandler {
    async fn handle(
        &self,
        msg: echo_agent::channels::InboundMessage,
    ) -> echo_core::error::Result<echo_agent::channels::OutboundMessage> {
        use futures::StreamExt;

        let channel_id = msg.channel_id.clone();
        let to = msg.reply_target().to_string();
        let chat_type = msg.chat_type;
        let mut stream = self.handle_stream(msg).await?;
        let mut reply = String::new();
        while let Some(item) = stream.next().await {
            let message = item?;
            if !reply.is_empty() && !reply.ends_with('\n') {
                reply.push('\n');
            }
            reply.push_str(&message.text);
        }
        Ok(echo_agent::channels::OutboundMessage::new(
            channel_id, to, chat_type, reply,
        ))
    }

    async fn handle_stream<'a>(
        &'a self,
        msg: echo_agent::channels::InboundMessage,
    ) -> echo_core::error::Result<
        futures::stream::BoxStream<
            'a,
            echo_core::error::Result<echo_agent::channels::OutboundMessage>,
        >,
    > {
        use echo_core::error::ChannelError;
        use futures::StreamExt;

        let immediate = match self.hitl.resolve_message(&msg.text).await {
            ChannelHumanLoopResolution::Resolved(message)
            | ChannelHumanLoopResolution::Invalid(message) => Some(message),
            ChannelHumanLoopResolution::NoPending => {
                parse_channel_mode_command(&msg.text, &self.interaction_mode).await
            }
        };
        if let Some(message) = immediate {
            let outbound = echo_agent::channels::OutboundMessage::new(
                &msg.channel_id,
                msg.reply_target(),
                msg.chat_type,
                message,
            );
            return Ok(futures::stream::once(async move { Ok(outbound) }).boxed());
        }

        let conv = Self::conversation_id(&msg.channel_id, msg.conversation_id());
        let cache_id = Self::cache_user_id(&msg.channel_id, msg.conversation_id());

        // 1. pool 取/复用 per-sender agent（bootstrap 等价全套已注入）
        let agent = self
            .pool
            .acquire(&conv)
            .await
            .map_err(|e| ChannelError::SendError(format!("AgentPool acquire failed: {e}")))?;

        // 2. 设 per-sender cache_user_id（写锁短暂）
        agent
            .write(|a| a.config_mut().set_cache_user_id(&cache_id))
            .await;
        let hitl = self.hitl.clone();
        agent
            .write_async(|agent| {
                Box::pin(async move {
                    agent.set_human_loop_provider_preserving_approvals(hitl);
                })
            })
            .await;
        if let Some(message) = channel_trace_response(&agent, &msg.text).await {
            let outbound = echo_agent::channels::OutboundMessage::new(
                &msg.channel_id,
                msg.reply_target(),
                msg.chat_type,
                message,
            );
            return Ok(futures::stream::once(async move { Ok(outbound) }).boxed());
        }
        if let Some(message) = channel_analysis_response(&agent, &msg.text).await {
            let outbound = echo_agent::channels::OutboundMessage::new(
                &msg.channel_id,
                msg.reply_target(),
                msg.chat_type,
                message,
            );
            return Ok(futures::stream::once(async move { Ok(outbound) }).boxed());
        }
        if let Some(message) = channel_papers_response(&agent, &msg.text).await {
            let outbound = echo_agent::channels::OutboundMessage::new(
                &msg.channel_id,
                msg.reply_target(),
                msg.chat_type,
                message,
            );
            return Ok(futures::stream::once(async move { Ok(outbound) }).boxed());
        }
        if let Some(message) = channel_skills_response(&agent, &msg.text).await {
            let outbound = echo_agent::channels::OutboundMessage::new(
                &msg.channel_id,
                msg.reply_target(),
                msg.chat_type,
                message,
            );
            return Ok(futures::stream::once(async move { Ok(outbound) }).boxed());
        }

        // 3. Drive through the shared `drive_chat` entry (TUI/GUI parity,
        //    AGENTS.md): route the message (normal vs complex) and stream
        //    versioned agent events to a channel sink; per-sender isolation means no
        //    concurrency, so the read guard is held for the stream lifetime
        //    inside `drive_chat` (same as TUI send_to_agent).
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<
            echo_agent_app_core::chat_driver::ChatDriverEvent,
        >();
        let text = msg.text.clone();
        // Persist IM attachments into the same durable reference contract as
        // GUI/TUI so TaskRuntime subagents can reconstruct the same message.
        let attachment_refs = stage_channel_attachments(&msg.attachments);
        let turn_id = uuid::Uuid::new_v4().to_string();
        // Channels have no workspace root; long pastes spill to the global
        // user-input artifact dir (~/.eko/artifacts/user-input/).
        let spill_dir = echo_agent_app_core::prepared_turn::resolve_user_input_spill_dir(None);
        let interaction_mode = *self.interaction_mode.read().await;
        let mode_hint_str = interaction_mode.prompt_hint().to_string();
        let turn = match echo_agent_app_core::prepared_turn::PreparedUserTurn::build(
            echo_agent_app_core::prepared_turn::UserTurnInput {
                text: &text,
                attachments: &attachment_refs,
                mode_hint: Some(&mode_hint_str),
                spill_dir: &spill_dir,
                conversation_id: Some(&conv),
                turn_id: Some(&turn_id),
            },
        ) {
            Ok(turn) => turn,
            Err(error) => {
                tracing::warn!(%error, conv = %conv, "channel user-turn preparation failed");
                let outbound = echo_agent::channels::OutboundMessage::new(
                    &msg.channel_id,
                    msg.reply_target(),
                    msg.chat_type,
                    format!("无法安全保存这条长消息，请检查本地磁盘后重试：{error}"),
                );
                return Ok(futures::stream::once(async move { Ok(outbound) }).boxed());
            }
        };
        let agent_owned = agent.clone();
        let pool = self.pool.clone();
        let store = self.store.clone();
        let review_integration = self.review_integration.clone();
        let webhook_emitter = self.webhook_emitter.clone();
        let mut prompt_rx = self.hitl.subscribe_prompts();
        let conv_owned = conv.clone();
        tokio::spawn(async move {
            use echo_agent_app_core::chat_driver::{ChannelChatSink, drive_chat};

            // 极简入口(Phase B1/B3):channel 不预判 normal/complex——agent 自主
            // 决定是否建后台 Run(create_complex_task,B3b)。ChatResources 经
            // drive_chat scope 进 task_local 供工具读。B5.4: multimodal 透传
            // IM 附件(图片/文件,与 GUI/TUI 同路径)。
            let cancel = echo_agent::agent::CancellationToken::new();
            let sink: std::sync::Arc<dyn echo_agent_app_core::chat_driver::ChatSink> =
                std::sync::Arc::new(ChannelChatSink::new(tx));
            let res = std::sync::Arc::new(echo_agent_app_core::chat_resources::ChatResources {
                pool: Some(pool),
                store,
                sink,
                webhook_emitter: Some(webhook_emitter),
                conv_id: Some(conv_owned.clone()),
                root_message_id: turn_id,
                attachments: turn.inline_attachment_refs(),
                cancel,
                mode_hint: Some(mode_hint_str),
                interaction_mode,
                layer_manager: review_integration
                    .as_ref()
                    .map(|integration| Arc::new(integration.create_layer_manager())),
            });
            if let Err(e) = drive_chat(&agent_owned, &turn, res).await {
                tracing::warn!(error = %e, conv = %conv_owned, "channel drive_chat failed");
            }
        });
        // Project the complete shared product stream into channel text.
        let event_stream = async_stream::stream! {
            let mut rx = rx;
            loop {
                tokio::select! {
                    event = rx.recv() => match event {
                        Some(event) => yield Ok(ChannelRenderEvent::Driver(event)),
                        None => break,
                    },
                    prompt = prompt_rx.recv() => match prompt {
                        Ok(prompt) => yield Ok(ChannelRenderEvent::Prompt(prompt)),
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                            tracing::warn!(skipped, "channel HITL prompt receiver lagged");
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {}
                    }
                }
            }
        }
        .boxed();

        // 4. 聚合成逐段 OutboundMessage 流
        let channel_id = msg.channel_id.clone();
        let to = msg.reply_target().to_string();
        let chat_type = msg.chat_type;
        Ok(aggregate_by_sentence(event_stream, channel_id, to, chat_type).await)
    }

    async fn reply(
        &self,
        _msg: echo_agent::channels::OutboundMessage,
    ) -> echo_core::error::Result<()> {
        // 实际发送由插件 wrapper（QqMessageHandler / FeishuMessageHandler）的 reply 承担
        //（wrapper 拦截 reply -> send_tx -> IM API）。inner reply 保持 no-op，
        // 与原 AgentChannelHandler::reply 一致。
        Ok(())
    }
}

#[cfg(feature = "channels")]
async fn parse_channel_mode_command(
    message: &str,
    mode: &tokio::sync::RwLock<echo_agent_app_core::tasks::task_runtime::InteractionMode>,
) -> Option<String> {
    use echo_agent_app_core::tasks::task_runtime::InteractionMode;

    let mut parts = message.split_whitespace();
    if parts.next()? != "/mode" {
        return None;
    }
    let Some(value) = parts.next() else {
        let current = mode.read().await;
        return Some(format!(
            "Current mode: {}. Usage: /mode chat|task|auto",
            current.as_str()
        ));
    };
    let next = match value.to_ascii_lowercase().as_str() {
        "chat" => InteractionMode::Chat,
        "task" => InteractionMode::Task,
        "auto" => InteractionMode::Auto,
        _ => return Some("Usage: /mode chat|task|auto".to_string()),
    };
    *mode.write().await = next;
    Some(format!("Interaction mode set to {}.", next.as_str()))
}

#[cfg(feature = "channels")]
async fn channel_trace_response(
    agent: &echo_agent_app_core::agent_handle::AgentHandle,
    message: &str,
) -> Option<String> {
    let mut parts = message.split_whitespace();
    if parts.next()? != "/trace" {
        return None;
    }
    let store = agent.read(|agent| agent.run_store.clone()).await;
    let Some(store) = store else {
        return Some("Run diagnostics are not configured.".to_string());
    };
    let diagnostic_id = match parts.next() {
        Some(value) => value.to_string(),
        None => {
            match echo_agent_app_core::observability::list_diagnostic_runs(store.as_ref()).await {
                Ok(runs) => match runs.first() {
                    Some(run) => run.diagnostic_id.clone(),
                    None => return Some("No durable run diagnostics available.".to_string()),
                },
                Err(error) => return Some(format!("Unable to list run diagnostics: {error}")),
            }
        }
    };
    Some(
        match echo_agent_app_core::observability::load_run_diagnostics(
            store.as_ref(),
            &diagnostic_id,
            None,
        )
        .await
        {
            Ok(Some(diagnostics)) => {
                echo_agent_app_core::observability::format_run_diagnostics(&diagnostics)
            }
            Ok(None) => format!("Run diagnostics not found: {diagnostic_id}"),
            Err(error) => format!("Unable to load run diagnostics: {error}"),
        },
    )
}

#[cfg(feature = "channels")]
async fn channel_analysis_response(
    agent: &echo_agent_app_core::agent_handle::AgentHandle,
    message: &str,
) -> Option<String> {
    let mut parts = message.split_whitespace();
    if parts.next()? != "/analysis" {
        return None;
    }
    let args: Vec<&str> = parts.collect();
    Some(crate::cli::cmd_impls::analysis::execute_analysis_command(agent, &args).await)
}

#[cfg(feature = "channels")]
async fn channel_papers_response(
    agent: &echo_agent_app_core::agent_handle::AgentHandle,
    message: &str,
) -> Option<String> {
    let mut parts = message.split_whitespace();
    if parts.next()? != "/papers" {
        return None;
    }
    let args: Vec<&str> = parts.collect();
    Some(crate::cli::cmd_impls::research::execute_papers_command(agent, &args).await)
}

#[cfg(feature = "channels")]
async fn channel_skills_response(
    agent: &echo_agent_app_core::agent_handle::AgentHandle,
    message: &str,
) -> Option<String> {
    let mut parts = message.split_whitespace();
    if parts.next()? != "/skills" {
        return None;
    }
    let args = parts.collect::<Vec<_>>();
    crate::cli::cmd_impls::skills::execute_skill_update_command(agent, &args).await
}

/// 将任意字符串清理为 DeepSeek `user_id` 合法形式 `[a-zA-Z0-9\-_]+`，最长 512 字符。
///
/// UTF-8 安全：用 `chars()` 迭代，禁止字节截断（中文/emoji → 替换为 `-`）。
/// 参考 AGENTS.md Rust 硬性约束 §1。
fn sanitize_cache_user_id(raw: &str) -> String {
    raw.chars()
        .take(512)
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

#[cfg(feature = "channels")]
fn channel_attachment_data(
    index: usize,
    attachment: &echo_agent::channels::MessageAttachment,
) -> echo_agent_app_core::types::AttachmentData {
    use base64::Engine as _;
    use echo_agent::channels::AttachmentKind;

    let fallback_name = match attachment.kind {
        AttachmentKind::Image => "image.png",
        AttachmentKind::File => "attachment.bin",
        AttachmentKind::Audio => "audio.bin",
        AttachmentKind::Video => "video.bin",
    };
    let name = attachment
        .filename
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("{}-{fallback_name}", index.saturating_add(1)));
    let inferred = echo_agent_app_core::attachments::infer_mime_type(&name);
    let mime_type = if inferred != "application/octet-stream" {
        inferred
    } else {
        match attachment.kind {
            AttachmentKind::Image => "image/png",
            AttachmentKind::File | AttachmentKind::Audio | AttachmentKind::Video => {
                "application/octet-stream"
            }
        }
    };
    echo_agent_app_core::types::AttachmentData {
        name,
        mime_type: mime_type.to_string(),
        data: base64::engine::general_purpose::STANDARD.encode(&attachment.data),
        size: u64::try_from(attachment.data.len()).unwrap_or(u64::MAX),
        source: echo_agent_app_core::types::AttachmentSource::Channel,
    }
}

#[cfg(feature = "channels")]
fn stage_channel_attachments(
    attachments: &[echo_agent::channels::MessageAttachment],
) -> Vec<echo_agent_app_core::attachments::AttachmentRef> {
    attachments
        .iter()
        .enumerate()
        .filter_map(|(index, attachment)| {
            let data = channel_attachment_data(index, attachment);
            match echo_agent_app_core::attachments::stage_attachment_data(&data, None) {
                Ok(reference) => Some(reference),
                Err(error) => {
                    tracing::warn!(%error, name = %data.name, "skipping channel attachment");
                    None
                }
            }
        })
        .collect()
}

#[cfg(feature = "channels")]
const FLUSH_THRESHOLD: usize = 80;

/// 句末标点(中英文)触发 flush。
#[cfg(feature = "channels")]
fn is_sentence_end(c: char) -> bool {
    // 中文句末:。 ． ！ ？ … ;英文句末:. ! ?
    matches!(c, '。' | '．' | '！' | '？' | '…' | '.' | '!' | '?')
}

/// 把共享 `ChatDriverEvent` 流按句/段落聚合成逐段 `OutboundMessage` 流。
///
/// flush 条件(满足任一):
/// 1. buf 含换行 → flush 到最后一个换行(含),保留换行后的剩余。
/// 2. buf 以句末标点结尾 → flush 全 buf。
/// 3. buf.chars().count() >= FLUSH_THRESHOLD → flush 全 buf。
///
/// 终态事件:FinalAnswer / Cancelled 先 flush 剩余 buf(若非空);
/// Error 先 flush 剩余后 yield Err;其它事件忽略。
///
/// 生命周期:返回流借用 'a(随 `events`),由 `try_stream!` 自然处理(宏生成的
/// future 持有 `events` 的借用)。UTF-8 安全:全用 chars() 判长和拆分
/// (AGENTS.md §1);无 unwrap/expect(§2)。
#[cfg(feature = "channels")]
async fn aggregate_by_sentence<'a>(
    mut events: futures::stream::BoxStream<'a, echo_core::error::Result<ChannelRenderEvent>>,
    channel_id: String,
    to: String,
    chat_type: echo_agent::channels::ChatType,
) -> futures::stream::BoxStream<'a, echo_core::error::Result<echo_agent::channels::OutboundMessage>>
{
    use echo_agent::channels::OutboundMessage;
    use echo_agent_app_core::chat_driver::ChatDriverEvent;
    use echo_core::agent::AgentEvent;
    use echo_core::error::{ChannelError, ReactError};
    use futures::StreamExt;

    let s = async_stream::try_stream! {
        let mut buf = String::new();
        // flush 全 buf(若非空)的统一动作,被多个终态/flush 分支共用。
        macro_rules! flush_all {
            () => {
                if !buf.is_empty() {
                    yield OutboundMessage::new(&channel_id, &to, chat_type, &buf);
                    buf.clear();
                }
            };
        }
        while let Some(ev) = events.next().await {
            match ev? {
                ChannelRenderEvent::Prompt(prompt) => {
                    flush_all!();
                    yield OutboundMessage::new(&channel_id, &to, chat_type, &prompt);
                }
                ChannelRenderEvent::Driver(ChatDriverEvent::Agent(envelope)) => match envelope.payload {
                AgentEvent::Token(t) => {
                    buf.push_str(&t);
                    // 1. 换行 flush(到最后一个 \n 含)。反向字符偏移表示换行后
                    //    还有多少字符,因此 `cut` 是包含换行的字符数。
                    if let Some(trailing_chars) = buf.chars().rev().position(|ch| ch == '\n') {
                        let cut = buf.chars().count().saturating_sub(trailing_chars);
                        let chunk: String = buf.chars().take(cut).collect();
                        buf = buf.chars().skip(cut).collect();
                        yield OutboundMessage::new(&channel_id, &to, chat_type, &chunk);
                    }
                    // 2/3. 句末标点 或 阈值(chars().count() 非字节)→ flush 全 buf
                    else if buf.chars().last().map(is_sentence_end).unwrap_or(false)
                        || buf.chars().count() >= FLUSH_THRESHOLD
                    {
                        flush_all!();
                    }
                }
                AgentEvent::FinalAnswer(_) => {
                    flush_all!();
                }
                AgentEvent::Cancelled => {
                    flush_all!();
                    break;
                }
                AgentEvent::Error { message, .. } => {
                    flush_all!();
                    Err(ReactError::Channel(Box::new(ChannelError::Other(format!(
                        "agent stream error: {message}"
                    )))))?;
                }
                AgentEvent::BudgetDecision { decision, reason, .. } => {
                    flush_all!();
                    yield OutboundMessage::new(
                        &channel_id,
                        &to,
                        chat_type,
                        format!("[budget] {decision:?}: {reason}"),
                    );
                }
                AgentEvent::GuardTriggered { guard, blocked } => {
                    flush_all!();
                    yield OutboundMessage::new(
                        &channel_id,
                        &to,
                        chat_type,
                        format!("[guard] {guard} (blocked={blocked})"),
                    );
                }
                AgentEvent::MemoryRecalled { count } => {
                    tracing::debug!(count, "channel agent recalled memory");
                }
                AgentEvent::Chart { spec } => {
                    flush_all!();
                    let preview: String = spec.to_string().chars().take(500).collect();
                    yield OutboundMessage::new(
                        &channel_id,
                        &to,
                        chat_type,
                        format!("[chart] {preview}"),
                    );
                }
                AgentEvent::SafetyNotice { action, reason, risk, permission } => {
                    flush_all!();
                    yield OutboundMessage::new(
                        &channel_id,
                        &to,
                        chat_type,
                        format!("[safety] {action}: {reason} (risk={risk}, permission={permission})"),
                    );
                }
                AgentEvent::ParameterError { tool, parameter, expected, got } => {
                    flush_all!();
                    yield OutboundMessage::new(
                        &channel_id,
                        &to,
                        chat_type,
                        format!("[parameter] {tool}.{parameter}: expected {expected}, got {got}"),
                    );
                }
                _ => {}
                },
                ChannelRenderEvent::Driver(ChatDriverEvent::Execution(event)) => {
                    if event.event.is_attention_event() {
                        flush_all!();
                        let detail: String = event.payload.to_string().chars().take(500).collect();
                        yield OutboundMessage::new(
                            &channel_id,
                            &to,
                            chat_type,
                            format!("[task:{}] {}: {detail}", event.run_id, event.event),
                        );
                    }
                }
                ChannelRenderEvent::Driver(ChatDriverEvent::TurnStatus { .. })
                | ChannelRenderEvent::Driver(ChatDriverEvent::ExecutionPath { .. }) => {}
                ChannelRenderEvent::Driver(ChatDriverEvent::Interrupt { run_id, goal, new_message }) => {
                    flush_all!();
                    yield OutboundMessage::new(
                        &channel_id,
                        &to,
                        chat_type,
                        format!("[paused:{run_id}] {goal}; new instruction: {new_message}"),
                    );
                }
            }
        }
    };
    s.boxed()
}

#[cfg(test)]
mod tests {
    use super::sanitize_cache_user_id;

    #[test]
    fn ascii_passthrough() {
        assert_eq!(
            sanitize_cache_user_id("im-qqbot-user_123"),
            "im-qqbot-user_123"
        );
    }

    #[test]
    fn chinese_replaced_with_dash() {
        // 输入 8 字符: i m - 飞 书 - 张 三
        // 字面 `-` 保留,4 个中文各替换为 `-` → im + 6 个 `-`
        assert_eq!(sanitize_cache_user_id("im-飞书-张三"), "im------");
    }

    #[test]
    fn emoji_and_specials_replaced() {
        assert_eq!(sanitize_cache_user_id("a@b.c🦀d"), "a-b-c-d");
    }

    #[test]
    fn truncated_to_512_chars() {
        let raw: String = "x".repeat(600);
        let out = sanitize_cache_user_id(&raw);
        assert_eq!(out.chars().count(), 512);
        assert!(out.chars().all(|c| c == 'x'));
    }

    #[test]
    fn empty_input_yields_empty() {
        assert_eq!(sanitize_cache_user_id(""), "");
    }

    #[test]
    fn conversation_id_format() {
        assert_eq!(
            super::AppChannelMessageHandler::conversation_id("qqbot", "user_123"),
            "channel:qqbot:user_123"
        );
    }

    #[test]
    fn cache_user_id_format() {
        assert_eq!(
            super::AppChannelMessageHandler::cache_user_id("qqbot", "user_123"),
            "im-qqbot-user_123"
        );
    }

    // ── channel attachment transport tests ──────────────────────────────
    #[cfg(feature = "channels")]
    mod multimodal {
        use super::super::channel_attachment_data;
        use echo_agent::channels::{AttachmentKind, MessageAttachment};

        #[test]
        fn image_attachment_keeps_name_and_image_mime() {
            let att = MessageAttachment::new(AttachmentKind::Image, vec![1, 2, 3])
                .with_filename("photo.png");
            let data = channel_attachment_data(0, &att);
            assert_eq!(data.name, "photo.png");
            assert_eq!(data.mime_type, "image/png");
            assert_eq!(data.size, 3);
        }

        #[test]
        fn file_attachment_keeps_inferred_text_mime() {
            let att = MessageAttachment::new(AttachmentKind::File, vec![9, 9, 9])
                .with_filename("notes.txt");
            let data = channel_attachment_data(0, &att);
            assert_eq!(data.name, "notes.txt");
            assert_eq!(data.mime_type, "text/plain");
            assert_eq!(data.size, 3);
        }
    }

    // ── aggregate_by_sentence 测试(需 channels feature)──────────────────────
    #[cfg(feature = "channels")]
    mod aggregate {
        use super::super::{ChannelRenderEvent, FLUSH_THRESHOLD, aggregate_by_sentence};
        use echo_agent::channels::{ChatType, OutboundMessage};
        use echo_core::agent::{AgentEvent, EventEnvelope, EventIdentity};
        use echo_core::error::Result;
        use futures::stream::{BoxStream, StreamExt};
        fn events_to_stream(
            events: Vec<Result<AgentEvent>>,
        ) -> BoxStream<'static, Result<ChannelRenderEvent>> {
            let identity = match EventIdentity::new("channel-test-stream", "channel-test") {
                Ok(identity) => identity,
                Err(error) => return futures::stream::once(async { Err(error) }).boxed(),
            };
            futures::stream::iter(events.into_iter().enumerate().map(move |(index, event)| {
                event.and_then(|payload| {
                    let sequence = u64::try_from(index).unwrap_or(u64::MAX).saturating_add(1);
                    EventEnvelope::new(&identity, sequence, None, payload).map(|envelope| {
                        ChannelRenderEvent::Driver(
                            echo_agent_app_core::chat_driver::ChatDriverEvent::Agent(Box::new(
                                envelope,
                            )),
                        )
                    })
                })
            }))
            .boxed()
        }

        async fn collect_texts(s: BoxStream<'_, Result<OutboundMessage>>) -> Vec<String> {
            let mut out = Vec::new();
            let mut s = s;
            while let Some(item) = s.next().await {
                match item {
                    Ok(m) => out.push(m.text),
                    Err(_) => break,
                }
            }
            out
        }

        #[tokio::test]
        async fn flush_on_newline() {
            // Token("a") Token("b\n") Token("c") FinalAnswer("") → "ab\n", "c"
            let evs = events_to_stream(vec![
                Ok(AgentEvent::Token("a".into())),
                Ok(AgentEvent::Token("b\n".into())),
                Ok(AgentEvent::Token("c".into())),
                Ok(AgentEvent::FinalAnswer(String::new())),
            ]);
            let out = aggregate_by_sentence(evs, "qq".into(), "u1".into(), ChatType::Direct).await;
            let texts = collect_texts(out).await;
            assert_eq!(texts, vec!["ab\n".to_string(), "c".to_string()]);
        }

        #[tokio::test]
        async fn flush_on_sentence_end() {
            // 中文句末标点 。 触发 flush
            let evs = events_to_stream(vec![
                Ok(AgentEvent::Token("你好。".into())),
                Ok(AgentEvent::Token("再见".into())),
                Ok(AgentEvent::FinalAnswer(String::new())),
            ]);
            let out = aggregate_by_sentence(evs, "qq".into(), "u1".into(), ChatType::Direct).await;
            let texts = collect_texts(out).await;
            assert_eq!(texts, vec!["你好。".to_string(), "再见".to_string()]);
        }

        #[tokio::test]
        async fn flush_on_threshold() {
            // 超过 FLUSH_THRESHOLD 字符阈值 flush(单 Token 阈值+10)
            let n = FLUSH_THRESHOLD + 10;
            let long: String = "x".repeat(n);
            let evs = events_to_stream(vec![
                Ok(AgentEvent::Token(long.clone())),
                Ok(AgentEvent::FinalAnswer(String::new())),
            ]);
            let out = aggregate_by_sentence(evs, "qq".into(), "u1".into(), ChatType::Direct).await;
            let texts = collect_texts(out).await;
            assert_eq!(texts.len(), 1, "threshold flush yields 1");
            assert_eq!(texts.first().map(|text| text.chars().count()), Some(n));
        }

        #[tokio::test]
        async fn finalanswer_flushes_remaining() {
            // 无标点的短串 + FinalAnswer flush 剩余
            let evs = events_to_stream(vec![
                Ok(AgentEvent::Token("hi".into())),
                Ok(AgentEvent::FinalAnswer(String::new())),
            ]);
            let out = aggregate_by_sentence(evs, "qq".into(), "u1".into(), ChatType::Direct).await;
            let texts = collect_texts(out).await;
            assert_eq!(texts, vec!["hi".to_string()]);
        }

        #[tokio::test]
        async fn empty_buf_finalanswer_no_yield() {
            // FinalAnswer 前无 Token → 不 yield 空
            let evs = events_to_stream(vec![Ok(AgentEvent::FinalAnswer(String::new()))]);
            let out = aggregate_by_sentence(evs, "qq".into(), "u1".into(), ChatType::Direct).await;
            let texts = collect_texts(out).await;
            assert!(texts.is_empty(), "no token before FinalAnswer → no yield");
        }

        #[tokio::test]
        async fn cancelled_flushes_remaining_then_stops() {
            let evs = events_to_stream(vec![
                Ok(AgentEvent::Token("partial".into())),
                Ok(AgentEvent::Cancelled),
            ]);
            let out = aggregate_by_sentence(evs, "qq".into(), "u1".into(), ChatType::Direct).await;
            let texts = collect_texts(out).await;
            assert_eq!(texts, vec!["partial".to_string()]);
        }

        #[tokio::test]
        async fn error_flushes_then_propagates() -> std::result::Result<(), String> {
            let evs = events_to_stream(vec![
                Ok(AgentEvent::Token("partial".into())),
                Ok(AgentEvent::error_message("llm", "boom")),
            ]);
            let out = aggregate_by_sentence(evs, "qq".into(), "u1".into(), ChatType::Direct).await;
            let mut s = out;
            // 第一条:flush 的 partial
            let first = s
                .next()
                .await
                .ok_or_else(|| "missing partial output".to_string())?
                .map_err(|error| error.to_string())?;
            assert_eq!(first.text, "partial");
            // 之后:Error 事件 → yield Err
            let second = s.next().await;
            assert!(second.is_some(), "error propagated as stream item");
            assert!(
                second.is_some_and(|item| item.is_err()),
                "error item is Err"
            );
            Ok(())
        }

        #[tokio::test]
        async fn multibyte_no_panic() {
            // 中文 + emoji 不 panic,按 FinalAnswer flush
            let evs = events_to_stream(vec![
                Ok(AgentEvent::Token("你好🦀世界".into())),
                Ok(AgentEvent::FinalAnswer(String::new())),
            ]);
            let out = aggregate_by_sentence(evs, "qq".into(), "u1".into(), ChatType::Direct).await;
            let texts = collect_texts(out).await;
            assert_eq!(texts, vec!["你好🦀世界".to_string()]);
        }

        #[tokio::test]
        async fn fullwidth_punctuation_flushes() {
            // 全角 ！ ？ 。 触发 flush(验证 is_sentence_end 全角分支)
            let evs = events_to_stream(vec![
                Ok(AgentEvent::Token("第一句！".into())),
                Ok(AgentEvent::Token("第二句？".into())),
                Ok(AgentEvent::FinalAnswer(String::new())),
            ]);
            let out = aggregate_by_sentence(evs, "qq".into(), "u1".into(), ChatType::Direct).await;
            let texts = collect_texts(out).await;
            assert_eq!(texts, vec!["第一句！".to_string(), "第二句？".to_string()]);
        }
    }
}
