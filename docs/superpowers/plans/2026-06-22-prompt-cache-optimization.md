# Prompt 缓存命中率优化实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 将 echo-agent 的 prompt 缓存命中率从 <1% 提升到 95%+ 级别（对标 Claude Code / Cursor / Codex），通过引入两层缓存架构（`PromptCacheLayout` 框架层 + `ProviderCacheAdapter` provider 适配层）统一产出稳定 prompt 结构并正确驱动各 provider 的缓存机制。

**Architecture:** 长期两层架构——
- **`PromptCacheLayout`（框架层）**：产出**稳定的、分段的** prompt 结构 `[system | canonical_context | tools_schema | conversation_history | runtime_context]`，保证前 4 段跨轮次字节稳定，`runtime_context` 作为可变尾部隔离。
- **`ProviderCacheAdapter`（适配层）**：把 layout 翻译成各 provider 的缓存协议——Anthropic 映射成 `cache_control` 断点（已有基础，重构统一），OpenAI-compatible 保持稳定 prefix + 传稳定 `user_id`（当前致命缺陷），解析 `cached_tokens`。
- **Provider-specific** 只补协议差异（断点位置、user_id 字段名、usage 字段名）。

**Tech Stack:** Rust / tokio / serde / async-trait（`echo-agent` + `echo-core` + `echo-integration` + `echo-state`）。

---

## 根因总览（已代码核实，写代码时直接引用）

| # | 位置 | 问题 | 影响 provider | 严重度 |
|---|---|---|---|---|
| **R1** | `react_loop.rs:45` | `ChatRequest { user_id: None, ... }` 硬编码 | OpenAI/DeepSeek/兼容 | **致命**——无稳定 user_id，KV-cache 每次当匿名用户，永不复用 |
| **R2** | `openai.rs:46-113` | 独立 `chat()`/`stream_chat()` 不接收 user_id 参数，第 72/107 行硬编码 `user_id: None` | OpenAI/DeepSeek | **致命**——结构性丢弃，调用方想传也传不进 |
| **R3** | `compression/mod.rs:634-650` | `reinject_canonical_context` 在 `pos=1` 插入 canonical 消息，整个对话历史后移 | 所有 | 高（间歇性，触发压缩时整段缓存失效） |
| **R4** | `context.rs:504-508` | 每轮 push 变动的 `[runtime_context:turn]` user message 到对话尾部（含 recalled memory） | OpenAI（无缓解）；Anthropic（已用 `is_runtime_context` 跳过） | 中 |
| **R5** | 无统一 layout | system / canonical / tools / history / runtime 五段混在 messages 数组里，无明确分段与断点策略，Anthropic 打补丁式设断点，OpenAI 完全无处理 | 所有 | 高（架构性） |

**已确认设计良好的部分（不动）：**
- `DEFAULT_AGENT_SYSTEM_PROMPT`（`config.rs:46`）是静态 `const &str`，无日期/UUID/cwd——缓存安全
- `ToolManager::get_openai_tools()`（`tools.rs:66`）按 `function.name` 排序并按 version 缓存——跨轮稳定
- Anthropic `apply_conversation_cache_breakpoints`（`anthropic.rs:809-843`）用 `is_runtime_context` 跳过尾部变动消息——方向正确
- Usage 解析（`types.rs:654-712`）已正确读取 OpenAI `cached_tokens` / Anthropic `cache_read_input_tokens` / DeepSeek `prompt_cache_hit_tokens`

---

## 文件结构（改动总览）

**新建：**
- `echo-core/src/llm/cache/layout.rs` — `PromptCacheLayout` 类型 + 分段组装逻辑
- `echo-core/src/llm/cache/adapter.rs` — `ProviderCacheAdapter` trait + 通用实现
- `echo-core/src/llm/cache/mod.rs` — 模块入口
- `echo-integration/src/providers/anthropic_cache.rs` — Anthropic 适配器（断点策略）
- `echo-integration/src/providers/openai_cache.rs` — OpenAI 兼容适配器（user_id + prefix）
- `echo-core/src/llm/cache/layout_tests.rs` — layout 稳定性单测

**修改：**
- `echo-core/src/llm/types.rs` — `ChatRequest` 增 `session_id`/`cache_layout` 字段；`Message` 增分段标记
- `echo-core/src/llm/mod.rs` — 暴露 `cache` 模块
- `echo-agent/src/agent/react/run/react_loop.rs` — 用 layout 组装请求，填 `user_id`
- `echo-agent/src/agent/react/run/context.rs` — runtime_context 改为可变尾部段，不再混入 history
- `echo-agent/src/agent/react/mod.rs` — session 启动时生成稳定 session_id 并注入
- `echo-state/src/compression/mod.rs` — `reinject_canonical_context` 改为追加到 canonical 段而非 pos=1 插入
- `echo-integration/src/providers/anthropic.rs` — `convert_request` 改用 `ProviderCacheAdapter`，删除打补丁式断点
- `echo-integration/src/providers/openai.rs` — `chat()`/`stream_chat()` 接收并透传 `user_id`；trait 实现改用 adapter
- `echo-integration/src/providers/mod.rs` — 暴露 cache 适配器

---

## 阶段 A：建立框架层 PromptCacheLayout（R5 根因）

### Task A1: 定义 PromptCacheLayout 类型与分段模型

**Files:**
- Create: `echo-core/src/llm/cache/mod.rs`
- Create: `echo-core/src/llm/cache/layout.rs`
- Modify: `echo-core/src/llm/mod.rs`（加 `pub mod cache;`）

- [ ] **Step 1: 新建 cache 模块入口**

创建 `echo-core/src/llm/cache/mod.rs`：
```rust
pub mod layout;
pub mod adapter;

pub use layout::{PromptCacheLayout, CacheSegment, SegmentKind};
pub use adapter::{ProviderCacheAdapter, CacheBreakpoint, AdaptedCachePlan};
```

在 `echo-core/src/llm/mod.rs` 模块声明区加：
```rust
pub mod cache;
```

- [ ] **Step 2: 定义分段类型**

创建 `echo-core/src/llm/cache/layout.rs`：
```rust
use crate::llm::types::{Message, Role, ToolDefinition};

/// prompt 缓存的稳定分段模型。
///
/// 五段顺序固定，前四段跨轮次字节稳定，`RuntimeContext` 为可变尾部：
///   [System | CanonicalContext | ToolsSchema | ConversationHistory | RuntimeContext]
///
/// 稳定段是 provider prefix cache 的命中基础；可变段必须隔离在末尾，
/// 且 provider 适配器需将其排除在缓存断点之外。
#[derive(Debug, Clone, Default)]
pub struct PromptCacheLayout {
    pub system: Vec<Message>,
    pub canonical_context: Vec<Message>,
    pub tools_schema: Vec<ToolDefinition>,
    pub conversation_history: Vec<Message>,
    pub runtime_context: Vec<Message>,
}

impl PromptCacheLayout {
    /// 把 layout 展平为发给 provider 的 messages 数组。
    ///
    /// 顺序：system → canonical_context → conversation_history → runtime_context。
    /// tools_schema 不进 messages（由 ChatRequest.tools 单独传），但参与 prefix 稳定性。
    pub fn flatten_messages(&self) -> Vec<Message> {
        let mut out = Vec::with_capacity(
            self.system.len()
                + self.canonical_context.len()
                + self.conversation_history.len()
                + self.runtime_context.len(),
        );
        out.extend(self.system.iter().cloned());
        out.extend(self.canonical_context.iter().cloned());
        out.extend(self.conversation_history.iter().cloned());
        out.extend(self.runtime_context.iter().cloned());
        out
    }

    /// 稳定段的字节哈希（用于断言跨轮稳定 / 调试缓存失效）。
    /// 仅 hash system + canonical + tools_schema + history，不含 runtime_context。
    pub fn stable_prefix_hash(&self) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut h = DefaultHasher::new();
        for m in &self.system { m.content.as_text().hash(&mut h); }
        for m in &self.canonical_context { m.content.as_text().hash(&mut h); }
        for t in &self.tools_schema {
            t.function.name.hash(&mut h);
            // parameters 用稳定序列化（sorted keys）
            let v = serde_json::to_string(&t.function.parameters).unwrap_or_default();
            v.hash(&mut h);
        }
        for m in &self.conversation_history { m.content.as_text().hash(&mut h); }
        h.finish()
    }
}

/// 单个缓存断点（provider 无关）。adapter 负责翻译成具体协议。
#[derive(Debug, Clone, Copy)]
pub struct CacheBreakpoint {
    /// 断点落在哪个段之后
    pub after: SegmentKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentKind {
    System,
    CanonicalContext,
    ToolsSchema,
    ConversationHistory,
    /// 可变尾部，通常不放断点（除"末尾追加缓存"模式）
    RuntimeContext,
}
```

- [ ] **Step 3: 类型检查编译**

Run: `cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent && cargo build -p echo-core`
Expected: 编译通过（`Message`/`ToolDefinition` 的字段引用需对照实际类型调整，见 Step 4）

- [ ] **Step 4: 校准字段访问**

若 `Message.content` 不是直接 `as_text()` 方法、或 `ToolDefinition.function` 结构不同，按 `echo-core/src/llm/types.rs` 实际定义调整 `stable_prefix_hash` 与 `flatten_messages` 的字段访问。以实际类型为准。

Run: `cargo build -p echo-core` 通过。

- [ ] **Step 5: 写 layout 稳定性单测**

在 `layout.rs` 末尾加：
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::types::{Message, Role};

    fn sys(t: &str) -> Message { Message::system(t.to_string()) }
    fn user(t: &str) -> Message { Message::user(t.to_string()) }

    #[test]
    fn flatten_preserves_segment_order() {
        let layout = PromptCacheLayout {
            system: vec![sys("S")],
            canonical_context: vec![sys("C")],
            tools_schema: vec![],
            conversation_history: vec![user("H1"), user("H2")],
            runtime_context: vec![user("R")],
        };
        let flat = layout.flatten_messages();
        assert_eq!(flat.len(), 5);
        assert_eq!(flat[0].content.as_text().unwrap(), "S");
        assert_eq!(flat[3].content.as_text().unwrap(), "H2");
        assert_eq!(flat[4].content.as_text().unwrap(), "R");
    }

    #[test]
    fn stable_prefix_hash_ignores_runtime_context() {
        let base = PromptCacheLayout {
            system: vec![sys("S")],
            canonical_context: vec![],
            tools_schema: vec![],
            conversation_history: vec![user("H")],
            runtime_context: vec![],
        };
        let mut with_rt = base.clone();
        with_rt.runtime_context = vec![user("changing-each-turn")];
        // runtime_context 变化不影响 stable prefix hash
        assert_eq!(base.stable_prefix_hash(), with_rt.stable_prefix_hash());
    }

    #[test]
    fn stable_prefix_hash_changes_when_history_changes() {
        let a = PromptCacheLayout {
            system: vec![sys("S")], canonical_context: vec![],
            tools_schema: vec![], conversation_history: vec![user("H1")],
            runtime_context: vec![],
        };
        let b = PromptCacheLayout { conversation_history: vec![user("H2")], ..a.clone() };
        assert_ne!(a.stable_prefix_hash(), b.stable_prefix_hash());
    }
}
```

> `Message::system`/`Message::user` 构造方式以实际 API 为准（可能是 `Message::new(Role::System, ...)`）；调整构造调用。

- [ ] **Step 6: 运行测试**

Run: `cargo test -p echo-core cache::layout`
Expected: 3 tests passed

- [ ] **Step 7: Commit**

```bash
cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent
git add echo-agent/echo-core/src/llm/cache/ echo-agent/echo-core/src/llm/mod.rs
git commit -m "feat(llm): add PromptCacheLayout segment model for stable prefix"
```

---

### Task A2: 定义 ProviderCacheAdapter trait

**Files:**
- Create: `echo-core/src/llm/cache/adapter.rs`

- [ ] **Step 1: 定义 trait 与适配计划类型**

创建 `echo-core/src/llm/cache/adapter.rs`：
```rust
use super::layout::{PromptCacheLayout, SegmentKind};

/// provider 缓存适配计划：adapter 根据 layout 产出 provider-specific 的缓存指令。
///
/// 这是"两层架构"的适配层入口：框架层只产出 layout（provider 无关），
/// adapter 负责翻译成 Anthropic cache_control 断点 / OpenAI user_id+prefix 等协议细节。
#[derive(Debug, Clone, Default)]
pub struct AdaptedCachePlan {
    /// 该 provider 是否需要 user_id（OpenAI 兼容族）
    pub user_id: Option<String>,
    /// 断点位置（Anthropic 族），按 layout 段表达；adapter 实现负责映射到具体消息索引
    pub breakpoints: Vec<SegmentKind>,
    /// 是否支持尾部追加缓存（OpenAI 自动前缀缓存，无需显式断点）
    pub implicit_prefix_cache: bool,
}

/// provider 缓存适配器。每个 provider 实现一次，把 layout 翻译成 AdaptedCachePlan。
pub trait ProviderCacheAdapter: Send + Sync {
    fn adapt(&self, layout: &PromptCacheLayout, session_id: &str) -> AdaptedCachePlan;
}
```

- [ ] **Step 2: 编译验证**

Run: `cargo build -p echo-core`
Expected: 通过

- [ ] **Step 3: Commit**

```bash
git add echo-agent/echo-core/src/llm/cache/adapter.rs
git commit -m "feat(llm): add ProviderCacheAdapter trait"
```

---

## 阶段 B：Provider 适配层实现（R1/R2/R5 根因）

### Task B1: OpenAI 兼容适配器（修 R1/R2 致命缺陷）

**Files:**
- Create: `echo-integration/src/providers/openai_cache.rs`
- Modify: `echo-integration/src/providers/mod.rs`（暴露模块）

- [ ] **Step 1: 新建 OpenAI 缓存适配器**

创建 `echo-integration/src/providers/openai_cache.rs`：
```rust
use echo_core::llm::cache::adapter::{AdaptedCachePlan, ProviderCacheAdapter};
use echo_core::llm::cache::layout::PromptCacheLayout;

/// OpenAI 兼容 provider 的缓存适配器。
///
/// OpenAI/DeepSeek/Qwen 等兼容 API 的 prompt caching 机制：
/// 1. 自动前缀缓存（implicit prefix cache）——请求的稳定前缀若与近期请求一致即命中
/// 2. 部分供应商（DeepSeek）用 `user` 字段做 KV-cache 分区关联，不稳定 user_id → 永不命中
///
/// 因此本适配器：
/// - 产出稳定的 `user_id`（基于 session_id），修复 R1/R2
/// - 不设显式断点（OpenAI 协议无 cache_control），靠 layout 保证前缀稳定
/// - 依赖 layout 把可变 runtime_context 隔离到尾部，前缀自然可缓存
pub struct OpenAiCacheAdapter;

impl ProviderCacheAdapter for OpenAiCacheAdapter {
    fn adapt(&self, _layout: &PromptCacheLayout, session_id: &str) -> AdaptedCachePlan {
        AdaptedCachePlan {
            // 稳定 user_id：同一会话内一致，跨会话可区分。
            // 不用随机值，不用递增计数，保证 KV-cache 分区稳定。
            user_id: Some(format!("echo-session-{session_id}")),
            breakpoints: vec![], // OpenAI 无显式断点协议
            implicit_prefix_cache: true,
        }
    }
}
```

- [ ] **Step 2: 暴露模块**

在 `echo-integration/src/providers/mod.rs` 加：
```rust
pub mod openai_cache;
pub use openai_cache::OpenAiCacheAdapter;
```

- [ ] **Step 3: 编译验证**

Run: `cargo build -p echo-integration`
Expected: 通过

- [ ] **Step 4: 写适配器单测**

在 `openai_cache.rs` 末尾加：
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn produces_stable_user_id_per_session() {
        let adapter = OpenAiCacheAdapter;
        let layout = PromptCacheLayout::default();
        let plan_a = adapter.adapt(&layout, "sess-123");
        let plan_b = adapter.adapt(&layout, "sess-123");
        assert_eq!(plan_a.user_id, plan_b.user_id);
        assert_eq!(plan_a.user_id.as_deref(), Some("echo-session-sess-123"));
        assert!(plan_a.implicit_prefix_cache);
        assert!(plan_a.breakpoints.is_empty());
    }

    #[test]
    fn different_sessions_different_user_id() {
        let adapter = OpenAiCacheAdapter;
        let layout = PromptCacheLayout::default();
        assert_ne!(
            adapter.adapt(&layout, "s1").user_id,
            adapter.adapt(&layout, "s2").user_id
        );
    }
}
```

- [ ] **Step 5: 运行测试**

Run: `cargo test -p echo-integration openai_cache`
Expected: 2 tests passed

- [ ] **Step 6: Commit**

```bash
git add echo-agent/echo-integration/src/providers/openai_cache.rs echo-agent/echo-integration/src/providers/mod.rs
git commit -m "feat(providers): add OpenAiCacheAdapter with stable user_id"
```

---

### Task B2: Anthropic 适配器（统一断点策略，替换打补丁逻辑）

**Files:**
- Create: `echo-integration/src/providers/anthropic_cache.rs`
- Modify: `echo-integration/src/providers/mod.rs`

- [ ] **Step 1: 新建 Anthropic 缓存适配器**

创建 `echo-integration/src/providers/anthropic_cache.rs`：
```rust
use echo_core::llm::cache::adapter::{AdaptedCachePlan, ProviderCacheAdapter};
use echo_core::llm::cache::layout::{PromptCacheLayout, SegmentKind};

/// Anthropic 缓存适配器。
///
/// Anthropic 支持 max 4 个 `cache_control: ephemeral` 断点。本适配器把 layout 段
/// 映射成断点位置，把"打补丁式"的断点逻辑（原 anthropic.rs:152-198）统一到这里：
///
/// 断点分配（最多 4 个）：
///   1. System 段末尾         —— system prompt 跨轮缓存
///   2. ToolsSchema 段末尾    —— tools 定义跨轮缓存（tools 变化时该断点失效，符合预期）
///   3. ConversationHistory ~75% 深度 —— 早期历史缓存
///   4. ConversationHistory 末尾      —— 最新消息下一轮命中
///
/// RuntimeContext 段**不放断点**且必须位于末尾——它是可变尾部，断点放这里会让
/// 下一轮的前缀匹配失败。这与原 `apply_conversation_cache_breakpoints` 的
/// `is_runtime_context` 跳过逻辑一致，但提升到 layout 层表达。
pub struct AnthropicCacheAdapter;

impl ProviderCacheAdapter for AnthropicCacheAdapter {
    fn adapt(&self, layout: &PromptCacheLayout, _session_id: &str) -> AdaptedCachePlan {
        let mut breakpoints = Vec::with_capacity(4);
        if !layout.system.is_empty() {
            breakpoints.push(SegmentKind::System);
        }
        if !layout.tools_schema.is_empty() {
            breakpoints.push(SegmentKind::ToolsSchema);
        }
        // history 断点只在历史足够长时才放（避免短对话浪费断点）
        if layout.conversation_history.len() >= 4 {
            breakpoints.push(SegmentKind::ConversationHistory);
            breakpoints.push(SegmentKind::ConversationHistory);
        }
        AdaptedCachePlan {
            user_id: None, // Anthropic 不用 user_id 做缓存分区
            breakpoints,
            implicit_prefix_cache: false,
        }
    }
}
```

- [ ] **Step 2: 暴露模块**

在 `echo-integration/src/providers/mod.rs` 加：
```rust
pub mod anthropic_cache;
pub use anthropic_cache::AnthropicCacheAdapter;
```

- [ ] **Step 3: 编译验证**

Run: `cargo build -p echo-integration`
Expected: 通过

- [ ] **Step 4: 写适配器单测**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use echo_core::llm::types::{Message, Role};

    fn user(t: &str) -> Message { Message::user(t.to_string()) }

    #[test]
    fn breakpoints_skip_runtime_context() {
        let adapter = AnthropicCacheAdapter;
        let layout = PromptCacheLayout {
            system: vec![Message::system("s")],
            canonical_context: vec![],
            tools_schema: vec![ToolDefinition::minimal("t1")],
            conversation_history: (0..6).map(|i| user(&format!("h{i}"))).collect(),
            runtime_context: vec![user("rt")],
        };
        let plan = adapter.adapt(&layout, "s");
        assert!(!plan.breakpoints.contains(&SegmentKind::RuntimeContext));
        assert!(plan.breakpoints.contains(&SegmentKind::System));
        assert!(plan.breakpoints.contains(&SegmentKind::ToolsSchema));
    }

    #[test]
    fn no_history_breakpoints_for_short_conversation() {
        let adapter = AnthropicCacheAdapter;
        let layout = PromptCacheLayout {
            system: vec![Message::system("s")],
            canonical_context: vec![],
            tools_schema: vec![],
            conversation_history: vec![user("only one")],
            runtime_context: vec![],
        };
        let plan = adapter.adapt(&layout, "s");
        assert!(!plan.breakpoints.contains(&SegmentKind::ConversationHistory));
    }

    // ToolDefinition::minimal 辅助构造，按实际 API 调整
}
```

> `ToolDefinition::minimal` 若不存在，用其真实构造 API 替换；或在测试里用一个最小合法 JSON schema 手工构造。

- [ ] **Step 5: 运行测试**

Run: `cargo test -p echo-integration anthropic_cache`
Expected: 2 tests passed

- [ ] **Step 6: Commit**

```bash
git add echo-agent/echo-integration/src/providers/anthropic_cache.rs echo-agent/echo-integration/src/providers/mod.rs
git commit -m "feat(providers): add AnthropicCacheAdapter unifying breakpoint strategy"
```

---

## 阶段 C：框架层接入 layout（R3/R4/R5 根因）

### Task C1: ChatRequest 增加 session_id 与 cache_layout 字段

**Files:**
- Modify: `echo-core/src/llm/types.rs:560-573`（ChatRequest 结构体）

- [ ] **Step 1: 扩展 ChatRequest**

在 `ChatRequest` 结构体（`types.rs:560` 附近）增加两个字段：
```rust
pub struct ChatRequest {
    // ...既有字段...
    pub user_id: Option<String>,
    /// 会话级稳定 ID，用作 provider 缓存分区的 user_id 来源。
    /// 由 agent 启动时生成，跨轮次不变。
    #[serde(default)]
    pub session_id: Option<String>,
    /// 预先计算好的缓存 layout（可选）。若提供，provider 实现应优先使用它
    /// 而非自行从 messages 推断断点。
    #[serde(default, skip_serializing, skip_deserializing)]
    pub cache_layout: Option<echo_core::llm::cache::PromptCacheLayout>,
}
```

> `skip_serializing` 因为 layout 是内存结构，不进 HTTP body（adapter 已消费它产出 plan）。

- [ ] **Step 2: 修复所有 ChatRequest 构造点**

Run: `cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent && cargo build 2>&1 | grep "missing field" | head -20`

对每个 `ChatRequest { ... }` 构造点补 `session_id: None, cache_layout: None`（后续 Task C3 会填真实值）。

- [ ] **Step 3: 全 workspace 编译**

Run: `cargo build`
Expected: 通过

- [ ] **Step 4: Commit**

```bash
git add echo-agent/echo-core/src/llm/types.rs
# 以及 Step 2 修复的其他文件
git commit -m "feat(llm): add session_id and cache_layout to ChatRequest"
```

---

### Task C2: agent 启动时生成稳定 session_id 并贯穿

**Files:**
- Modify: `echo-agent/src/agent/react/mod.rs`（ReactAgent 结构体 + build/start）
- Modify: `echo-agent/src/agent/react/builder.rs`

- [ ] **Step 1: ReactAgent 增 session_id 字段**

在 `ReactAgent` 结构体定义中加：
```rust
/// 会话级稳定 ID，用于 provider prompt cache 分区。整个 agent 生命周期不变。
session_id: String,
```

- [ ] **Step 2: 构造时生成 session_id**

在 builder 的 `build()` 或 agent 构造处：
```rust
let session_id = format!(
    "{:016x}",
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u128
);
```

> 用纳秒时间戳而非 `Uuid::new_v4()`——避免引入新依赖；同一会话内稳定即可，跨会话唯一性靠纳秒精度足够。若项目已依赖 uuid，直接用 `Uuid::new_v4().to_string()`。

- [ ] **Step 3: 暴露 accessor**

```rust
impl ReactAgent {
    pub fn session_id(&self) -> &str { &self.session_id }
}
```

- [ ] **Step 4: 编译验证**

Run: `cargo build -p echo-agent`
Expected: 通过

- [ ] **Step 5: Commit**

```bash
git add echo-agent/src/agent/react/mod.rs echo-agent/src/agent/react/builder.rs
git commit -m "feat(agent): generate stable session_id per agent lifetime"
```

---

### Task C3: react_loop 用 layout 组装请求并填 user_id（修 R1）

**Files:**
- Modify: `echo-agent/src/agent/react/run/react_loop.rs:36-46`

- [ ] **Step 1: 构造 PromptCacheLayout 并填入 ChatRequest**

定位 `react_loop.rs:36` 的 `ChatRequest { ... }`。改为：
```rust
// 从 context manager 获取分段后的消息（Task C4 会实现分段）
let layout = PromptCacheLayout {
    system: /* Task C4 提供 */,
    canonical_context: /* Task C4 提供 */,
    tools_schema: tools.clone(),
    conversation_history: /* Task C4 提供 */,
    runtime_context: /* Task C4 提供 */,
};
let cache_plan = self.cache_adapter.as_ref()
    .map(|a| a.adapt(&layout, &self.session_id));

let request = ChatRequest {
    messages: layout.flatten_messages(),
    temperature,
    max_tokens,
    tools: Some(tools.clone()),
    tool_choice: None,
    response_format: response_format.clone(),
    thinking: self.thinking.clone(),
    cancel_token: None,
    user_id: cache_plan.as_ref().and_then(|p| p.user_id.clone()),
    session_id: Some(self.session_id.clone()),
    cache_layout: Some(layout),
};
```

> `self.cache_adapter` 字段在 Task C5 加入。本步骤先写好调用形态，C5 补字段。

- [ ] **Step 2: 暂用占位 adapter 字段让编译通过**

若 `self.cache_adapter` 还不存在，本步骤先在 ReactAgent 加 `cache_adapter: Option<Box<dyn ProviderCacheAdapter>>` 字段（C5 填充），构造时暂设 `None`，让 `cache_plan` 为 `None`、`user_id` 为 `None`——保证编译通过但行为未变（C5 注入真实 adapter 后才生效）。

Run: `cargo build -p echo-agent` 通过。

- [ ] **Step 3: Commit**

```bash
git add echo-agent/src/agent/react/run/react_loop.rs echo-agent/src/agent/react/mod.rs
git commit -m "refactor(react): assemble ChatRequest via PromptCacheLayout"
```

---

### Task C4: context manager 输出分段 layout（修 R3/R4）

**Files:**
- Modify: `echo-agent/src/agent/react/run/context.rs`（prepare_stream_context + prepare）
- Modify: `echo-agent/echo-state/src/compression/mod.rs:614-651`（reinject_canonical_context）

- [ ] **Step 1: context manager 维护分段而非单一 messages 数组**

这是本计划最核心的结构改动。当前 `self.memory.context` 是单一 `Vec<Message>`，system/canonical/history/runtime 全混在一起。改为维护分段结构。

在 `ContextManager`（或等价结构）增加分段字段：
```rust
pub(crate) struct ContextSegments {
    pub system: Vec<Message>,
    pub canonical_context: Vec<Message>,
    pub conversation_history: Vec<Message>,
    pub runtime_context: Vec<Message>,  // 每轮清空重建
}
```

`prepare_stream_context`（`context.rs:473`）改为：
- 不再把 `runtime_context_note` push 到 `context`（即原 history）
- 改为写入 `segments.runtime_context`（每轮重置）
- user input push 到 `segments.conversation_history`

```rust
// context.rs:502-508 改为：
let mut segments = self.memory.segments.lock().await;
segments.runtime_context.clear();  // 每轮重置可变尾部
segments.conversation_history.push(Message::user(input.to_string()));
if let Some(runtime_context) = format_turn_runtime_context(memory_context.as_deref(), ws_block.as_str()) {
    segments.runtime_context.push(runtime_context_note("turn", &runtime_context));
}
```

- [ ] **Step 2: 提供 layout 组装方法**

```rust
impl ContextManager {
    pub(crate) fn build_cache_layout(&self, tools: Vec<ToolDefinition>) -> PromptCacheLayout {
        let seg = self.memory.segments.blocking_lock();
        PromptCacheLayout {
            system: seg.system.clone(),
            canonical_context: seg.canonical_context.clone(),
            tools_schema: tools,
            conversation_history: seg.conversation_history.clone(),
            runtime_context: seg.runtime_context.clone(),
        }
    }
}
```

> `react_loop` 调此方法获得 layout（替换 C3 Step 1 中的占位）。

- [ ] **Step 3: 修复 reinject_canonical_context（R3）**

`compression/mod.rs:634-650` 当前在 `pos=1` 插入 canonical 消息，移动整个 history。改为**追加到 canonical 段**而非插入 history 中间：

```rust
fn reinject_canonical_context(&mut self) {
    let Some(ref canonical) = self.canonical_context else { return; };

    // system 段缺失则补到 system 段头部
    if self.segments.system.is_empty() {
        if let Some(ref prompt) = canonical.system_prompt {
            self.segments.system.insert(0, Message::system(prompt.clone()));
        }
    }

    // canonical context 追加到 canonical 段（不插到 history 中间！）
    if let Some(msgs) = canonical.to_reinjection_messages() {
        self.segments.canonical_context.extend(msgs.into_iter().map(Message::system));
    }
}
```

> 关键改动：从 `messages.insert(pos=1, ...)` 改为 `segments.canonical_context.extend(...)`。canonical 段在 history 之前，追加不会移动 history，前缀稳定。

- [ ] **Step 4: 适配所有读取 `self.memory.context` 的地方**

Run: `cargo build 2>&1 | grep "no field\|cannot find" | head -30`
对每个访问旧 `context` 字段的地方，改为访问对应 segment（system/canonical/history 之一）。典型：
- 压缩器读取 history → `segments.conversation_history`
- 审计/导出读取全部 → `flatten` 三段

- [ ] **Step 5: 全 workspace 编译**

Run: `cargo build`
Expected: 通过

- [ ] **Step 6: 运行既有测试确认无回归**

Run: `cargo test`
Expected: 既有测试全 pass（部分测试可能因字段重命名需更新构造代码）

- [ ] **Step 7: Commit**

```bash
git add echo-agent/src/agent/react/run/context.rs echo-agent/echo-state/src/compression/mod.rs
# 以及 Step 4 修复的其他文件
git commit -m "refactor(context): segment messages into cache-stable layout, fix reinject position"
```

---

### Task C5: ReactAgent 注入 cache_adapter（修 R1/R5 完成）

**Files:**
- Modify: `echo-agent/src/agent/react/mod.rs` + `builder.rs`

- [ ] **Step 1: ReactAgent 持有 cache_adapter**

在 ReactAgent 结构体加（C3 Step 2 已加占位字段，本步骤填值）：
```rust
cache_adapter: Option<Box<dyn ProviderCacheAdapter>>,
```

- [ ] **Step 2: builder 根据 provider 类型注入对应 adapter**

在 `builder.rs` 的 build 逻辑里，根据 model 配置的 provider family 选择：
```rust
let cache_adapter: Option<Box<dyn ProviderCacheAdapter>> = match model.provider_family() {
    ProviderFamily::Anthropic => Some(Box::new(AnthropicCacheAdapter)),
    ProviderFamily::OpenAiCompatible => Some(Box::new(OpenAiCacheAdapter)),
    ProviderFamily::Unknown => None, // 不支持的 provider 不做缓存适配
};
```

> `provider_family()` 若不存在，在 model config 加一个判断方法（按 baseurl 或 model name 前缀判断 anthropic vs openai-compatible）。

- [ ] **Step 3: 编译验证**

Run: `cargo build -p echo-agent`
Expected: 通过

- [ ] **Step 4: Commit**

```bash
git add echo-agent/src/agent/react/mod.rs echo-agent/src/agent/react/builder.rs
git commit -m "feat(agent): inject ProviderCacheAdapter based on provider family"
```

---

## 阶段 D：Provider 实现消费 layout + cache_plan（修 R2）

### Task D1: OpenAI provider 消费 user_id（修 R2 致命缺陷）

**Files:**
- Modify: `echo-integration/src/providers/openai.rs:46-113`（独立 chat/stream_chat）
- Modify: `echo-integration/src/providers/openai.rs:185-205`（trait chat 实现）

- [ ] **Step 1: 独立 chat() 接收 user_id 参数**

`openai.rs:46` 的 `chat()` 签名增加 `user_id: Option<String>` 参数，第 72 行改为 `user_id: user_id.clone()`：
```rust
pub async fn chat(
    client: Arc<Client>,
    model_name: &str,
    messages: &[Message],
    temperature: Option<f32>,
    max_tokens: Option<u32>,
    stream: Option<bool>,
    tools: Option<Vec<ToolDefinition>>,
    tool_choice: Option<String>,
    response_format: Option<ResponseFormat>,
    user_id: Option<String>,  // ← 新增
) -> Result<ChatCompletionResponse> {
    // ...
    let request_body = ChatCompletionRequest {
        // ...
        user_id,  // ← 原 None 改为透传
    };
```

- [ ] **Step 2: stream_chat() 同样接收 user_id**

`openai.rs:81` 的 `stream_chat()` 同理加 `user_id: Option<String>` 参数，第 107 行改 `user_id`。

- [ ] **Step 3: 修复所有 chat()/stream_chat() 调用点**

Run: `cargo build 2>&1 | grep "missing field\|arguments" | head -20`
对每个调用点补传 `user_id`（从 `ChatRequest.user_id` 或 `cache_plan.user_id` 取）。

- [ ] **Step 4: trait chat 实现已正确（204 行透传），无需改**

确认 `OpenAiClient::chat`（trait 实现，185-205 行）已 `user_id: request.user_id.clone()`，无需改动。

- [ ] **Step 5: 编译验证**

Run: `cargo build`
Expected: 通过

- [ ] **Step 6: 运行测试**

Run: `cargo test -p echo-integration`
Expected: 既有测试全 pass

- [ ] **Step 7: Commit**

```bash
git add echo-agent/echo-integration/src/providers/openai.rs
# 以及调用点修复
git commit -m "fix(providers): thread user_id through OpenAI chat/stream_chat (R2)"
```

---

### Task D2: OpenAI trait chat 实现消费 cache_layout（可选优化）

**Files:**
- Modify: `echo-integration/src/providers/openai.rs:185-214`

- [ ] **Step 1: 验证 layout 已通过 flatten_messages 进入 request.messages**

经 C3 改造，`request.messages` 已是 `layout.flatten_messages()` 的结果，OpenAI 自动前缀缓存会匹配稳定前缀。trait 实现无需额外处理 layout（OpenAI 协议无 cache_control）。

本步骤为**验证步骤**而非代码改动：确认 `request.messages` 顺序为 `[system, canonical, history, runtime]`，且 `request.user_id` 非空。

- [ ] **Step 2: 加日志便于观测缓存命中**

在 trait chat 实现的请求发出前加：
```rust
tracing::debug!(
    user_id = ?request.user_id,
    msg_count = request.messages.len(),
    stable_prefix_hash = ?request.cache_layout.as_ref().map(|l| l.stable_prefix_hash()),
    "openai request cache trace"
);
```

- [ ] **Step 3: 编译 + Commit**

```bash
cargo build -p echo-integration
git add echo-agent/echo-integration/src/providers/openai.rs
git commit -m "feat(providers): log stable prefix hash for OpenAI cache observability"
```

---

### Task D3: Anthropic provider 改用 adapter 断点（替换打补丁逻辑）

**Files:**
- Modify: `echo-integration/src/providers/anthropic.rs:150-198`（convert_request）

- [ ] **Step 1: convert_request 消费 cache_plan.breakpoints**

定位 `anthropic.rs:152` 的 `convert_request`。当前手动设 system/tool/conversation 三处断点。改为从 `request.cache_layout` + `AnthropicCacheAdapter` 读取断点段，映射到消息索引：

```rust
// 替换原 152-198 的断点逻辑
let cache_plan = request.cache_layout.as_ref().map(|layout| {
    let adapter = AnthropicCacheAdapter;
    adapter.adapt(layout, request.session_id.as_deref().unwrap_or(""))
});

// system 断点
let system = system.map(|text| {
    let has_sys_bp = cache_plan.as_ref()
        .map(|p| p.breakpoints.contains(&SegmentKind::System))
        .unwrap_or(true);
    AnthropicSystem::Blocks(vec![SystemBlock {
        block_type: "text".to_string(),
        text,
        cache_control: if has_sys_bp { Some(CacheControl::ephemeral()) } else { None },
    }])
});

// tools 断点（最后一个 tool）
let tools: Option<Vec<AnthropicToolDef>> = request.tools.as_ref().map(|tools| {
    let count = tools.len();
    let has_tool_bp = cache_plan.as_ref()
        .map(|p| p.breakpoints.contains(&SegmentKind::ToolsSchema))
        .unwrap_or(true);
    tools.iter().enumerate().map(|(i, t)| AnthropicToolDef {
        name: t.function.name.clone(),
        description: Some(t.function.description.clone()),
        input_schema: t.function.parameters.clone(),
        cache_control: if has_tool_bp && i == count - 1 {
            Some(CacheControl::ephemeral())
        } else { None },
    }).collect()
});

// conversation 断点：从 cache_plan.breakpoints 含 ConversationHistory 的数量决定放几处
let conv_bp_count = cache_plan.as_ref()
    .map(|p| p.breakpoints.iter().filter(|s| **s == SegmentKind::ConversationHistory).count())
    .unwrap_or(2);
apply_conversation_cache_breakpoints(&mut messages, conv_bp_count);
```

- [ ] **Step 2: 保留 apply_conversation_cache_breakpoints 的 is_runtime_context 跳过逻辑**

该函数（`anthropic.rs:809`）的 `is_runtime_context` 跳过逻辑仍然正确（runtime_context 在末尾，断点跳过它）。保留不动，只是断点数量现在由 adapter 决定。

- [ ] **Step 3: 编译验证**

Run: `cargo build -p echo-integration`
Expected: 通过

- [ ] **Step 4: 运行测试**

Run: `cargo test -p echo-integration`
Expected: 既有 anthropic 测试全 pass

- [ ] **Step 5: Commit**

```bash
git add echo-agent/echo-integration/src/providers/anthropic.rs
git commit -m "refactor(providers): Anthropic uses ProviderCacheAdapter breakpoints"
```

---

## 阶段 E：验证与可观测

### Task E1: 缓存命中率埋点与诊断日志

**Files:**
- Modify: `echo-agent/src/agent/react/run/stream_channel.rs:268-303`（LlmUsage 事件）
- Modify: `echo-agent/src/agent/react/run/phases/think.rs:162`（LlmUsage 事件）

- [ ] **Step 1: 在 LlmUsage 事件附加缓存命中率字段**

当前 `AgentEvent::LlmUsage` 已有 `cached_prompt_tokens`/`cache_creation_prompt_tokens`。在发送该事件处增加命中率计算日志：
```rust
let cache_hit_rate = if prompt_tokens + cached_prompt_tokens > 0 {
    cached_prompt_tokens as f64 / (prompt_tokens + cached_prompt_tokens) as f64
} else { 0.0 };
tracing::info!(
    agent = %self.config.agent_name,
    prompt_tokens, completion_tokens, cached_prompt_tokens,
    cache_creation_prompt_tokens, usage_reported,
    cache_hit_rate = format!("{:.1}%", cache_hit_rate * 100.0),
    "💰 prompt cache stats"
);
```

- [ ] **Step 2: 阶段性汇总日志（每 N 轮）**

在 ContextManager 或 ReactAgent 维护一个累计计数器，每 10 轮输出一次累计命中率：
```rust
tracing::info!(
    turns = total_turns,
    cumulative_cache_hit_rate = format!("{:.1}%", cum_hit_rate * 100.0),
    "📊 cumulative cache performance"
);
```

- [ ] **Step 3: 编译 + Commit**

```bash
cargo build -p echo-agent
git add echo-agent/src/agent/react/run/stream_channel.rs echo-agent/src/agent/react/run/phases/think.rs
git commit -m "feat(observability): log prompt cache hit rate per turn and cumulative"
```

---

### Task E2: layout 稳定性断言（防回归）

**Files:**
- Create: `echo-core/src/llm/cache/layout_tests.rs`
- Modify: `echo-core/src/llm/cache/mod.rs`（`#[cfg(test)] mod layout_tests;`）

- [ ] **Step 1: 跨轮 stable_prefix_hash 稳定性测试**

```rust
#[cfg(test)]
mod layout_tests {
    use super::*;
    // ...construct two layouts simulating turn N and turn N+1...
    // turn N+1 only appends to conversation_history and resets runtime_context
    // assert stable_prefix_hash changes ONLY due to history growth, not due to
    // system/canonical/tools reordering or content drift
}
```

具体测试：构造 turn N 的 layout，复制为 turn N+1，仅在 `conversation_history` 追加一条、`runtime_context` 重置。断言：
- system/canonical/tools 段 hash 完全一致
- 整体 stable_prefix_hash 因 history 增长而变（符合预期）
- runtime_context 变化不影响 stable_prefix_hash

- [ ] **Step 2: 运行测试 + Commit**

Run: `cargo test -p echo-core cache::layout_tests`
```bash
git add echo-agent/echo-core/src/llm/cache/layout_tests.rs
git commit -m "test(cache): assert stable prefix hash invariance across turns"
```

---

## 验收清单（全部完成后端到端验证）

- [ ] **R1 修复**：`react_loop.rs` 的 `ChatRequest.user_id` 来自 `cache_plan.user_id`，非 None
- [ ] **R2 修复**：`openai.rs` 的 `chat()`/`stream_chat()` 接收并透传 `user_id`，无硬编码 None
- [ ] **R3 修复**：`reinject_canonical_context` 追加到 canonical 段，不再 `insert(pos=1)`
- [ ] **R4 修复**：`runtime_context_note` 写入 `segments.runtime_context`，不混入 history
- [ ] **R5 修复**：`PromptCacheLayout` 五段模型落地，两个 provider 适配器实现 `ProviderCacheAdapter`
- [ ] **缓存命中率**：同模型同任务连续 3 轮，日志显示 `cache_hit_rate` 从首轮 0% 升至 95%+（Anthropic）/ 90%+（OpenAI 兼容）
- [ ] **回归**：`cargo build` 全 workspace 通过；`cargo test` 全 pass；常规单 agent 对话功能正常
- [ ] **可观测**：每轮日志输出 `cache_hit_rate`，每 10 轮输出累计命中率

## 业界对照（设计依据）

- **Claude Code**：用 Anthropic `cache_control` 4 断点（system/tools/2x history），命中率 98%+。本计划 `AnthropicCacheAdapter` 复刻此策略，提升到 layout 层表达。
- **Cursor / Codex**：OpenAI 兼容族，靠稳定 prefix + 稳定 user_id 命中自动前缀缓存。本计划 `OpenAiCacheAdapter` 修复 user_id 缺陷 + layout 保证 prefix 稳定。
- **OpenCode / OpenClaw**：同样依赖 prefix 稳定性，核心是 system/tools/history 不抖动。本计划 layout 强制分段隔离可变尾部。
- **共同点**：所有优秀实现都把"可变内容（时间戳/cwd/recall）"隔离到 prompt 末尾或单独通道，绝不混入前缀。本计划 `runtime_context` 段即此原则的落地。
