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
#[cfg(feature = "channels")]
pub struct AppChannelMessageHandler {
    pool: Arc<AgentPool>,
}

#[cfg(feature = "channels")]
impl AppChannelMessageHandler {
    pub fn new(pool: Arc<AgentPool>) -> Self {
        Self { pool }
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

        // 3. 非流式 chat（Phase 1.2 切 chat_stream）。read 锁跨 chat：per-sender 无并发，
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
}
