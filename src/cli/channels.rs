//! 应用层 IM channel 消息处理器 —— 把 IM 消息桥接到 `AgentPool`。
//!
//! 与框架层 `AgentChannelHandler::standard`（裸建 `ReactAgent`，不经 bootstrap）不同，
//! 此处 agent 从 `AgentPool::acquire` 取，经 `AgentRuntime::bootstrap` 全套接通
//! （state_store / store / compressor / MemoryLayerManager / permission_service /
//! cache_user_id / conversation_id）。per-sender 隔离由 pool key
//! `channel:{channel_id}:{sender_id}` 承载，对齐 Claude Code "one session per user"。
//!
//! 归属（spec §D1-6）：`AgentPool` 是 EKO 产品概念，handler 放应用层（bin crate），
//! 不进框架 `channels.rs`。框架 `AgentChannelHandler::new` 保留供纯框架/测试用。

#[cfg(feature = "channels")]
use std::sync::Arc;

#[cfg(feature = "channels")]
use echo_agent_app_core::agent_pool::AgentPool;

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
}

#[cfg(feature = "channels")]
impl AppChannelMessageHandler {
    pub fn new(
        pool: Arc<AgentPool>,
        store: Option<Arc<echo_agent_app_core::tasks::task_runtime::TaskRuntimeStore>>,
    ) -> Self {
        Self { pool, store }
    }

    /// per-sender conversation_id（= pool key）。sender 维度隔离。
    fn conversation_id(channel_id: &str, sender_id: &str) -> String {
        format!("channel:{channel_id}:{sender_id}")
    }

    /// per-sender cache_user_id（DeepSeek KVCache 隔离 + 隐私）。
    fn cache_user_id(channel_id: &str, sender_id: &str) -> String {
        sanitize_cache_user_id(&format!("im-{channel_id}-{sender_id}"))
    }
}

#[cfg(feature = "channels")]
#[async_trait::async_trait]
impl echo_agent::channels::MessageHandler for AppChannelMessageHandler {
    async fn handle(
        &self,
        msg: echo_agent::channels::InboundMessage,
    ) -> echo_core::error::Result<echo_agent::channels::OutboundMessage> {
        use echo_agent::agent::Agent; // 提供 ReactAgent::chat（Agent trait 方法）
        use echo_core::error::ChannelError;

        let conv = Self::conversation_id(&msg.channel_id, &msg.sender_id);
        let cache_id = Self::cache_user_id(&msg.channel_id, &msg.sender_id);

        // 1. 从 pool 取/复用 per-sender agent（bootstrap 等价全套已注入）。
        //    PoolError -> ChannelError::SendError，再经 From<ChannelError> for ReactError 转。
        let agent = self
            .pool
            .acquire(&conv)
            .await
            .map_err(|e| ChannelError::SendError(format!("AgentPool acquire failed: {e}")))?;

        // 2. 设 per-sender cache_user_id（写锁短暂持有，不跨 chat）。
        agent
            .write(|a| a.config_mut().set_cache_user_id(&cache_id))
            .await;

        // 3. 非流式 chat。read 锁跨 chat：per-sender 无并发，
        //    pool cleanup monitor 用 try_read 见忙即不驱逐（与 TUI repl.rs:394 同 pattern）。
        let guard = agent.inner().read().await;
        let reply = guard.chat(&msg.text).await?;

        Ok(echo_agent::channels::OutboundMessage::new(
            &msg.channel_id,
            &msg.sender_id,
            msg.chat_type,
            &reply,
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

        let conv = Self::conversation_id(&msg.channel_id, &msg.sender_id);
        let cache_id = Self::cache_user_id(&msg.channel_id, &msg.sender_id);

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

        // 3. Drive through the shared `drive_chat` entry (TUI/GUI parity,
        //    AGENTS.md): route the message (normal vs complex) and stream
        //    versioned agent events to a channel sink; per-sender isolation means no
        //    concurrency, so the read guard is held for the stream lifetime
        //    inside `drive_chat` (same as TUI send_to_agent).
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<echo_agent::agent::EventEnvelope>();
        let text = msg.text.clone();
        // B5.4: convert IM-channel attachments (QQ/飞书 images/files) into a
        // multimodal Message so the agent sees them — same path the GUI and
        // TUI /attach use. None when there are no attachments (plain text turn).
        let multimodal = build_channel_multimodal_message(&text, &msg.attachments);
        let agent_owned = agent.clone();
        let pool = self.pool.clone();
        let store = self.store.clone();
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
                conv_id: Some(conv_owned.clone()),
                root_message_id: uuid::Uuid::new_v4().to_string(),
                attachments: vec![],
                cancel,
                // IM channels are always Auto (no mode selector): prompt-level
                // classification, not a runtime state machine.
                mode_hint: Some(
                    echo_agent_app_core::tasks::task_runtime::InteractionMode::Auto
                        .prompt_hint()
                        .to_string(),
                ),
                interaction_mode: echo_agent_app_core::tasks::task_runtime::InteractionMode::Auto,
                // B5.1: channels have no review/memory subsystem; autonomous run
                // memory writes are no-ops (recall closure off).
                layer_manager: None,
            });
            if let Err(e) = drive_chat(&agent_owned, &text, multimodal.as_ref(), res).await {
                tracing::warn!(error = %e, conv = %conv_owned, "channel drive_chat failed");
            }
        });
        // Project each shared envelope payload into the existing sentence
        // aggregator. Identity and ordering were already validated by drive_chat,
        // which also normalized stream errors into terminal payloads.
        let event_stream = futures::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|item| (Ok(item.payload), rx))
        })
        .boxed();

        // 4. 聚合成逐段 OutboundMessage 流
        let channel_id = msg.channel_id.clone();
        let to = msg.sender_id.clone();
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

/// Build a multimodal user `Message` from an IM channel's text + attachments
/// (B5.4). Images become `ContentPart::ImageUrl` (inline base64 data URL); all
/// other kinds (File/Audio/Video) become `ContentPart::File`. Returns `None`
/// when there are no attachments (plain text turn — zero overhead, drive_chat
/// builds `Message::user(text)` itself).
#[cfg(feature = "channels")]
fn build_channel_multimodal_message(
    text: &str,
    attachments: &[echo_agent::channels::MessageAttachment],
) -> Option<echo_core::llm::types::Message> {
    use base64::Engine as _;
    use echo_agent::channels::AttachmentKind;
    use echo_agent::llm::types::{ContentPart, ImageUrl};

    if attachments.is_empty() {
        return None;
    }
    let mut parts = Vec::with_capacity(attachments.len() + 1);
    parts.push(ContentPart::Text {
        text: text.to_string(),
    });
    for att in attachments {
        let b64 = base64::engine::general_purpose::STANDARD.encode(&att.data);
        match att.kind {
            AttachmentKind::Image => {
                // Image MIME inferred from the kind; providers parse the data URL.
                parts.push(ContentPart::ImageUrl {
                    image_url: ImageUrl {
                        url: format!("data:image/png;base64,{b64}"),
                        detail: None,
                    },
                });
            }
            AttachmentKind::File | AttachmentKind::Audio | AttachmentKind::Video => {
                let name = att
                    .filename
                    .clone()
                    .unwrap_or_else(|| "attachment".to_string());
                parts.push(ContentPart::File { name, content: b64 });
            }
        }
    }
    Some(echo_agent::llm::types::Message::user_multimodal(parts))
}

#[cfg(feature = "channels")]
const FLUSH_THRESHOLD: usize = 80;

/// 句末标点(中英文)触发 flush。
#[cfg(feature = "channels")]
fn is_sentence_end(c: char) -> bool {
    // 中文句末:。 ． ！ ？ … ;英文句末:. ! ?
    matches!(c, '。' | '．' | '！' | '？' | '…' | '.' | '!' | '?')
}

/// 把 `AgentEvent` 流按句/段落聚合成逐段 `OutboundMessage` 流。
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
/// future 持有 `events` 的借用)。UTF-8 安全:全用 chars() 判长(AGENTS.md §1);
/// 无 unwrap/expect(§2);换行切片见下方注释。
#[cfg(feature = "channels")]
async fn aggregate_by_sentence<'a>(
    mut events: futures::stream::BoxStream<
        'a,
        echo_core::error::Result<echo_core::agent::AgentEvent>,
    >,
    channel_id: String,
    to: String,
    chat_type: echo_agent::channels::ChatType,
) -> futures::stream::BoxStream<'a, echo_core::error::Result<echo_agent::channels::OutboundMessage>>
{
    use echo_agent::channels::OutboundMessage;
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
                AgentEvent::Token(t) => {
                    buf.push_str(&t);
                    // 1. 换行 flush(到最后一个 \n 含)
                    //    安全:`\n` 是 ASCII 单字节,`rfind('\n')` 返字节 idx,
                    //    其位置必在完整 UTF-8 字符边界(\n 不出现在多字节字符中间),
                    //    故 buf[..cut] / buf[cut..] 切在字符边界,不会 panic。
                    if let Some(idx) = buf.rfind('\n') {
                        let cut = idx + '\n'.len_utf8(); // = idx + 1
                        let chunk: String = buf[..cut].to_string();
                        buf = buf[cut..].to_string();
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
                _ => {} // ToolCall/ThinkStart/LlmUsage 等本 Phase 忽略
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

    // ── build_channel_multimodal_message 测试(需 channels feature)─────────
    #[cfg(feature = "channels")]
    mod multimodal {
        use super::super::build_channel_multimodal_message;
        use echo_agent::channels::{AttachmentKind, MessageAttachment};
        use echo_core::llm::types::{ContentPart, MessageContent};

        #[test]
        fn no_attachments_returns_none() {
            assert!(build_channel_multimodal_message("hi", &[]).is_none());
        }

        #[test]
        fn image_attachment_becomes_image_url() {
            let att = MessageAttachment::new(AttachmentKind::Image, vec![1, 2, 3])
                .with_filename("photo.png");
            let msg =
                build_channel_multimodal_message("look", &[att]).expect("Some for attachments");
            let parts = match &msg.content {
                MessageContent::Parts(p) => p.clone(),
                other => panic!("expected Parts, got {other:?}"),
            };
            assert_eq!(parts.len(), 2, "text + 1 image");
            assert!(matches!(parts[0], ContentPart::Text { .. }));
            match &parts[1] {
                ContentPart::ImageUrl { image_url } => {
                    assert!(image_url.url.starts_with("data:image/png;base64,"));
                }
                other => panic!("expected ImageUrl, got {other:?}"),
            }
        }

        #[test]
        fn file_attachment_becomes_file_part() {
            let att = MessageAttachment::new(AttachmentKind::File, vec![9, 9, 9])
                .with_filename("notes.txt");
            let msg =
                build_channel_multimodal_message("see", &[att]).expect("Some for attachments");
            let parts = match &msg.content {
                MessageContent::Parts(p) => p.clone(),
                other => panic!("expected Parts, got {other:?}"),
            };
            match &parts[1] {
                ContentPart::File { name, .. } => assert_eq!(name, "notes.txt"),
                other => panic!("expected File, got {other:?}"),
            }
        }
    }

    // ── aggregate_by_sentence 测试(需 channels feature)──────────────────────
    #[cfg(feature = "channels")]
    mod aggregate {
        use super::super::{FLUSH_THRESHOLD, aggregate_by_sentence};
        use echo_agent::channels::{ChatType, OutboundMessage};
        use echo_core::agent::AgentEvent;
        use echo_core::error::Result;
        use futures::stream::{BoxStream, StreamExt};
        fn events_to_stream(
            events: Vec<Result<AgentEvent>>,
        ) -> BoxStream<'static, Result<AgentEvent>> {
            futures::stream::iter(events).boxed()
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
            assert_eq!(texts[0].chars().count(), n);
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
        async fn error_flushes_then_propagates() {
            let evs = events_to_stream(vec![
                Ok(AgentEvent::Token("partial".into())),
                Ok(AgentEvent::Error {
                    source: "llm".into(),
                    message: "boom".into(),
                }),
            ]);
            let out = aggregate_by_sentence(evs, "qq".into(), "u1".into(), ChatType::Direct).await;
            let mut s = out;
            // 第一条:flush 的 partial
            let first = s.next().await.unwrap().unwrap();
            assert_eq!(first.text, "partial");
            // 之后:Error 事件 → yield Err
            let second = s.next().await;
            assert!(second.is_some(), "error propagated as stream item");
            assert!(second.unwrap().is_err(), "error item is Err");
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
