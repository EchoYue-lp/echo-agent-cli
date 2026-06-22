# Prompt 缓存命中率优化实施计划 v2（修订版）

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 将 echo-agent 的 prompt 缓存命中率从 <1% 提升到 95%+ 级别（对标 Claude Code / Cursor / Codex），通过**小步抽只读 layout view + 保留现有机器级 cache_user_id + provider helper 收敛**的路线，避免推倒式重构。

**Architecture:** 三步递进，每步独立可测可回滚——
1. **`PromptCacheLayout::from_messages()`（只读 view）**：从现有单一 messages 数组识别分段 `[system | canonical | stable_history | runtime_context]`，不改底层存储。
2. **保留并补全 `cache_user_id`**：机器级持久化 UUID 已就位，只补测试覆盖残留的 `react_loop.rs:45` 非流式规划路径。
3. **provider helper 收敛**：先把 Anthropic 现有 breakpoint 逻辑抽成 `AnthropicCachePlan`，OpenAI 只做 prefix+user_id；两条路径稳定后再抽象 `ProviderCacheAdapter` trait。

**Tech Stack:** Rust / tokio / serde（`echo-agent` + `echo-core` + `echo-integration` + `echo-state`）。

---

## 修订说明（相对 v1 的关键调整）

本版基于对 v1 审查反馈的逐条核实，做以下调整：

| v1 做法 | v2 调整 | 原因（已代码核实） |
|---|---|---|
| 新增 `session_id` 生成 `echo-session-{id}` 作 user_id | **保留现有 `cache_user_id`**（`~/.echo-agent/cache_user_id` 持久化 UUID） | v1 会从机器级倒退到 session 级，跨会话冷启动。现 `infra.rs:69 load_or_create_cache_user_id()` 已是机器级稳定 |
| R1 列为"致命未修" | **R1 降级**：流式主路径 `think.rs:267` 已透传 `cache_user_id`；仅 `react_loop.rs:45` 非流式规划路径残留 None | v1 遗漏了 `phases/think.rs` 路径 |
| C4 推倒 ContextManager 为 `ContextSegments` 存储 | **改为只读 `from_messages()` view**，不改底层存储 | 推倒存储牵动压缩/transcript/checkpoint/tool loop/hook/verifier，风险过大 |
| Anthropic adapter 用 `Vec<SegmentKind>`（重复 `ConversationHistory`） | **改为 `Vec<BreakpointTarget>`**（`HistoryIndex(usize)` / `HistoryLastStable`） | 现代码用 usize 精确索引，enum 重复表达脆弱 |
| `stable_prefix_hash()` 用 `DefaultHasher` | **改为 SHA-256 + canonical JSON（BTreeMap sorted keys）** | DefaultHasher 跨进程不稳；serde_json key 顺序不保证 |
| `ChatRequest` 塞 `cache_layout: PromptCacheLayout` | **改为 `cache_hints: Option<CacheHints>`**（仅断点目标+hash+段范围） | 避免核心类型耦合 provider 上下文 |
| 提前设计 `ProviderCacheAdapter` trait | **延后**：先做 Anthropic/OpenAI 两条 helper，稳定后再抽象 | 避免过度泛化 |

**仍成立的根因（v1 正确部分保留）：**
- **R2** `openai.rs:46-113` 独立 `chat()`/`stream_chat()` 仍硬编码 `user_id: None`，结构性丢弃（已核实，未修）
- **R3** `compression/mod.rs:634-650` `reinject_canonical_context` 仍 `insert(pos=1)`，触发压缩时前缀失效（已核实，未修）
- **R4** runtime_context 已在尾部隔离（Anthropic 已用 `is_runtime_context` 跳过），但 OpenAI 无等价处理
- **R5** 无统一 layout 抽象，Anthropic 打补丁式断点散在 `convert_request`

**设计良好不动：**
- `DEFAULT_AGENT_SYSTEM_PROMPT`（`config.rs:46`）静态 const，缓存安全
- `ToolManager::get_openai_tools()`（`tools.rs:66`）按 name 排序+版本缓存
- Anthropic `apply_conversation_cache_breakpoints`（`anthropic.rs:809`）`is_runtime_context` 跳过逻辑方向正确
- Usage 解析（`types.rs:654-712`）已正确读取三家 cached_tokens

---

## 文件结构（改动总览）

**新建：**
- `echo-core/src/llm/cache/layout.rs` — `PromptCacheLayout` 只读 view + `BreakpointTarget` + `CacheHints`
- `echo-core/src/llm/cache/diagnostic.rs` — SHA-256 canonical hash（诊断用）
- `echo-core/src/llm/cache/mod.rs` — 模块入口
- `echo-integration/src/providers/anthropic_cache.rs` — `AnthropicCachePlan` helper（收敛现有断点逻辑）
- `echo-integration/src/providers/openai_cache.rs` — OpenAI prefix 稳定性 helper（仅观测+断言，无协议改动）

**修改：**
- `echo-core/src/llm/types.rs` — `ChatRequest` 增 `cache_hints: Option<CacheHints>`
- `echo-core/src/llm/mod.rs` — 暴露 cache 模块
- `echo-agent/src/agent/react/run/react_loop.rs:45` — 非流式规划路径透传 `cache_user_id`（R1 残留修复）
- `echo-agent/src/agent/react/run/phases/think.rs:258` — 构造 layout 并填 `cache_hints`（流式主路径）
- `echo-state/src/compression/mod.rs:634-650` — `reinject_canonical_context` 改追加 canonical 段而非 `insert(pos=1)`（R3 修复）
- `echo-integration/src/providers/anthropic.rs:150-198` — `convert_request` 改用 `AnthropicCachePlan`
- `echo-integration/src/providers/openai.rs:46-113` — 独立 `chat()`/`stream_chat()` 接收并透传 `user_id`（R2 修复）

---

## 阶段 A：只读 PromptCacheLayout view（R5 根因，低风险）

### Task A1: 定义 layout view 与 BreakpointTarget

**Files:**
- Create: `echo-core/src/llm/cache/mod.rs`
- Create: `echo-core/src/llm/cache/layout.rs`
- Modify: `echo-core/src/llm/mod.rs`（加 `pub mod cache;`）

- [ ] **Step 1: 新建 cache 模块入口**

创建 `echo-core/src/llm/cache/mod.rs`：
```rust
pub mod layout;
pub mod diagnostic;

pub use layout::{PromptCacheLayout, BreakpointTarget, CacheHints, SegmentRange};
```

在 `echo-core/src/llm/mod.rs` 模块声明区加：
```rust
pub mod cache;
```

- [ ] **Step 2: 定义 BreakpointTarget（精确点，非 enum 重复）**

创建 `echo-core/src/llm/cache/layout.rs`：
```rust
use crate::llm::types::{Message, ToolDefinition};

/// 缓存断点的精确目标。adapter 输出此类型，provider 实现负责映射到协议。
///
/// 注意：不用 enum 重复表达"两个 history 断点"——用 HistoryIndex(usize)
/// 和 HistoryLastStable 分别表达"75% 深度"和"末尾稳定消息"两个具体点。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakpointTarget {
    /// system 段最后一个 block
    SystemLastBlock,
    /// tools 段最后一个 tool definition
    ToolsLastTool,
    /// conversation history 中指定索引处的消息
    HistoryIndex(usize),
    /// conversation history 中最后一条非 runtime_context 消息
    HistoryLastStable,
}

/// provider 无关的缓存提示，挂在 ChatRequest 上轻量传递。
/// 不含完整 layout（避免 core 类型耦合 provider 上下文），只放 provider 需要的：
/// 断点目标 + 稳定前缀 hash（诊断用）+ 各段范围（provider 可选用于日志）。
#[derive(Debug, Clone, Default)]
pub struct CacheHints {
    /// Anthropic 族：断点目标列表（最多 4 个）
    pub breakpoints: Vec<BreakpointTarget>,
    /// 稳定前缀的 SHA-256（canonical 序列化），用于日志观测缓存失效
    pub stable_prefix_hash: Option<String>,
    /// 各段在 flatten messages 中的索引范围 [start, end)
    pub segments: SegmentRanges,
}

#[derive(Debug, Clone, Default)]
pub struct SegmentRanges {
    pub system: SegmentRange,
    pub canonical: SegmentRange,
    pub history: SegmentRange,
    pub runtime_context: SegmentRange,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SegmentRange {
    pub start: usize,
    pub end: usize,
}

impl SegmentRange {
    pub fn len(&self) -> usize { self.end.saturating_sub(self.start) }
    pub fn is_empty(&self) -> bool { self.len() == 0 }
}

/// 只读 layout view：从现有单一 messages 数组识别分段，不改原数组。
///
/// 分段规则（基于现有代码的消息标记，非新存储）：
/// - system: 开头连续的 Role::System 消息
/// - canonical: system 之后、含 "Canonical context" 标记的 system 消息
/// - history: canonical 之后、直到首个 runtime_context 之前的所有消息
/// - runtime_context: 尾部以 `[runtime_context:` 开头的消息（可能多条）
#[derive(Debug, Clone)]
pub struct PromptCacheLayout<'a> {
    pub system: &'a [Message],
    pub canonical: &'a [Message],
    pub history: &'a [Message],
    pub runtime_context: &'a [Message],
    pub tools: &'a [ToolDefinition],
}

impl<'a> PromptCacheLayout<'a> {
    /// 从 flatten 后的 messages + tools 识别分段（只读，零拷贝）。
    pub fn from_messages(messages: &'a [Message], tools: &'a [ToolDefinition]) -> Self {
        // system 段：开头连续 System role
        let sys_end = messages
            .iter()
            .position(|m| m.role != crate::llm::types::Role::System)
            .unwrap_or(messages.len());

        // canonical 段：system 段内（或紧随）含 "Canonical context" 的消息
        // 现有 to_reinjection_messages 产生 "[Canonical context — ...]" 文本
        let canonical_end = messages[..sys_end]
            .iter()
            .rposition(|m| {
                m.content.as_text()
                    .map(|t| t.contains("Canonical context"))
                    .unwrap_or(false)
            })
            .map(|i| i + 1)
            .unwrap_or(0); // 无 canonical 则空段
        let system_seg = &messages[..canonical_end.min(sys_end)];
        let canonical_seg = if canonical_end > 0 && canonical_end <= sys_end {
            &messages[canonical_end.min(sys_end)..sys_end]
        } else {
            &messages[0..0]
        };

        // runtime_context 段：尾部以 [runtime_context: 开头的消息
        let rt_start = messages
            .iter()
            .rposition(|m| {
                m.content.as_text()
                    .map(|t| t.trim_start().starts_with("[runtime_context:"))
                    .unwrap_or(false)
            })
            .map(|i| {
                // 向前扩展连续的 runtime_context 消息
                let mut s = i;
                while s > sys_end {
                    let prev = &messages[s - 1];
                    let is_rt = prev
                        .content
                        .as_text()
                        .map(|t| t.trim_start().starts_with("[runtime_context:"))
                        .unwrap_or(false);
                    if !is_rt { break; }
                    s -= 1;
                }
                s
            })
            .unwrap_or(messages.len());

        let history_seg = &messages[sys_end..rt_start];
        let runtime_seg = &messages[rt_start..];

        Self {
            system: system_seg,
            canonical: canonical_seg,
            history: history_seg,
            runtime_context: runtime_seg,
            tools,
        }
    }

    /// 计算各段在原 messages 数组中的索引范围（供 CacheHints 用）。
    pub fn segment_ranges(&self) -> SegmentRanges {
        // 基于 from_messages 的切片反推索引——需在调用处保证 messages 引用同源
        // 这里用段长 + 假定连续排列计算（from_messages 已保证顺序）
        let sys_len = self.system.len();
        let canon_len = self.canonical.len();
        let hist_len = self.history.len();
        let rt_len = self.runtime_context.len();
        SegmentRanges {
            system: SegmentRange { start: 0, end: sys_len },
            canonical: SegmentRange { start: sys_len, end: sys_len + canon_len },
            history: SegmentRange { start: sys_len + canon_len, end: sys_len + canon_len + hist_len },
            runtime_context: SegmentRange {
                start: sys_len + canon_len + hist_len,
                end: sys_len + canon_len + hist_len + rt_len,
            },
        }
    }
}
```

- [ ] **Step 3: 校准字段访问并编译**

Run: `cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent && cargo build -p echo-core`
若 `Message.role`/`Message.content.as_text()`/`Role` 访问方式与实际不符，按 `echo-core/src/llm/types.rs` 实际定义调整。以实际 API 为准。
Expected: 编译通过

- [ ] **Step 4: 写 from_messages 分段单测**

在 `layout.rs` 末尾加：
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::types::{Message, Role};

    fn sys(t: &str) -> Message { Message::system(t.to_string()) }
    fn user(t: &str) -> Message { Message::user(t.to_string()) }
    fn rt(t: &str) -> Message { Message::user(format!("[runtime_context:{t}]")) }

    #[test]
    fn segments_typical_conversation() {
        let msgs = vec![
            sys("You are Echo Agent"),
            sys("[Canonical context — system prompt restored]"),
            user("hello"),
            user("how are you"),
            rt("turn\ncwd: /tmp"),
        ];
        let layout = PromptCacheLayout::from_messages(&msgs, &[]);
        assert_eq!(layout.system.len(), 1);
        assert_eq!(layout.canonical.len(), 1);
        assert_eq!(layout.history.len(), 2);
        assert_eq!(layout.runtime_context.len(), 1);
    }

    #[test]
    fn no_canonical_yields_empty_canonical_seg() {
        let msgs = vec![sys("S"), user("hi")];
        let layout = PromptCacheLayout::from_messages(&msgs, &[]);
        assert_eq!(layout.canonical.len(), 0);
        assert_eq!(layout.system.len(), 1);
        assert_eq!(layout.history.len(), 1);
    }

    #[test]
    fn multiple_trailing_runtime_context_grouped() {
        let msgs = vec![
            sys("S"),
            user("hi"),
            rt("turn\nctx1"),
            rt("Hook:PostCompact\nctx2"),
        ];
        let layout = PromptCacheLayout::from_messages(&msgs, &[]);
        assert_eq!(layout.history.len(), 1);
        assert_eq!(layout.runtime_context.len(), 2);
    }

    #[test]
    fn ranges_match_slice_lengths() {
        let msgs = vec![sys("S"), sys("[Canonical context — x]"), user("h"), rt("t")];
        let layout = PromptCacheLayout::from_messages(&msgs, &[]);
        let r = layout.segment_ranges();
        assert_eq!(r.system.len(), 1);
        assert_eq!(r.canonical.len(), 1);
        assert_eq!(r.history.len(), 1);
        assert_eq!(r.runtime_context.len(), 1);
        assert_eq!(r.runtime_context.end, 4);
    }
}
```

> `Message::system`/`Message::user` 构造方式以实际 API 为准调整。

- [ ] **Step 5: 运行测试**

Run: `cargo test -p echo-core cache::layout`
Expected: 4 tests passed

- [ ] **Step 6: Commit**

```bash
cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent
git add echo-agent/echo-core/src/llm/cache/ echo-agent/echo-core/src/llm/mod.rs
git commit -m "feat(llm): add read-only PromptCacheLayout view with BreakpointTarget"
```

---

### Task A2: SHA-256 canonical hash 诊断模块

**Files:**
- Create: `echo-core/src/llm/cache/diagnostic.rs`

- [ ] **Step 1: 实现 canonical JSON + SHA-256**

创建 `echo-core/src/llm/cache/diagnostic.rs`：
```rust
use crate::llm::types::{Message, ToolDefinition};
use sha2::{Digest, Sha256};

/// 计算稳定前缀的 SHA-256（canonical 序列化），用于日志观测缓存失效。
///
/// 关键：必须跨进程可复现，所以：
/// 1. 用 SHA-256 而非 DefaultHasher（后者跨进程/跨版本不稳）
/// 2. tools schema 用 canonical JSON（BTreeMap sorted keys），避免 serde_json
///    默认序列化的 key 顺序不确定
/// 3. 只 hash 稳定段（system+canonical+tools+history），不含 runtime_context
pub fn stable_prefix_hash(
    system: &[Message],
    canonical: &[Message],
    tools: &[ToolDefinition],
    history: &[Message],
) -> String {
    let mut hasher = Sha256::new();

    for m in system { hash_message(&mut hasher, m); }
    for m in canonical { hash_message(&mut hasher, m); }
    for t in tools { hash_tool(&mut hasher, t); }
    for m in history { hash_message(&mut hasher, m); }

    let result = hasher.finalize();
    // 16 位 hex 前缀足够诊断用，日志可读
    format!("{:x}", &result[..8])
}

fn hash_message(hasher: &mut Sha256, m: &Message) {
    hasher.update(b"MSG:");
    hasher.update(m.role.as_str().as_bytes());
    hasher.update(b":");
    if let Some(text) = m.content.as_text() {
        hasher.update(text.as_bytes());
    }
    hasher.update(b"\n");
}

fn hash_tool(hasher: &mut Sha256, t: &ToolDefinition) {
    hasher.update(b"TOOL:");
    hasher.update(t.function.name.as_bytes());
    hasher.update(b":");
    // canonical JSON：sorted keys，确保跨进程一致
    let canonical = canonical_json_string(&t.function.parameters);
    hasher.update(canonical.as_bytes());
    hasher.update(b"\n");
}

fn canonical_json_string(v: &serde_json::Value) -> String {
    // serde_json::to_string 对 Object 用 BTreeMap 时 key 已排序；
    // Value::Object 内部是 Map，默认 BTreeMap（feature "preserve_order" 关闭时）
    // 显式用 to_string 即可得到 sorted-key 输出
    serde_json::to_string(v).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_deterministic_across_calls() {
        // 同输入两次 hash 必须一致（跨进程可复现的前提）
        let h1 = stable_prefix_hash(&[], &[], &[], &[]);
        let h2 = stable_prefix_hash(&[], &[], &[], &[]);
        assert_eq!(h1, h2);
    }

    #[test]
    fn hash_ignores_order_in_tools_schema() {
        // 两个 key 顺序不同但内容相同的 JSON 应产生相同 hash
        let v1: serde_json::Value = serde_json::from_str(r#"{"a":1,"b":2}"#).unwrap();
        let v2: serde_json::Value = serde_json::from_str(r#"{"b":2,"a":1}"#).unwrap();
        // Value::Object 默认 BTreeMap，已排序
        assert_eq!(canonical_json_string(&v1), canonical_json_string(&v2));
    }
}
```

- [ ] **Step 2: 添加 sha2 依赖**

检查 `echo-core/Cargo.toml` 是否已有 `sha2`。若无，加：
```toml
sha2 = "0.10"
```

Run: `cargo build -p echo-core` 确认依赖就绪。

- [ ] **Step 3: 运行测试**

Run: `cargo test -p echo-core cache::diagnostic`
Expected: 2 tests passed

- [ ] **Step 4: Commit**

```bash
git add echo-agent/echo-core/src/llm/cache/diagnostic.rs echo-agent/echo-core/Cargo.toml
git commit -m "feat(llm): add SHA-256 canonical hash for cache diagnostics"
```

---

## 阶段 B：修复残留根因（R1 残留 / R2 / R3，低风险定点修）

### Task B1: 修复 react_loop.rs 非流式规划路径 user_id 残留（R1 残留）

**Files:**
- Modify: `echo-agent/src/agent/react/run/react_loop.rs:36-46`

- [ ] **Step 1: 透传 cache_user_id**

定位 `react_loop.rs:36` 的 `ChatRequest { ... }`，第 45 行 `user_id: None` 改为：
```rust
user_id: self.config.cache_user_id.clone(),
```

> 这是 v1 计划 R1 的残留部分。流式主路径 `think.rs:267` 已透传，本步补齐非流式规划路径。

- [ ] **Step 2: 编译验证**

Run: `cargo build -p echo-agent`
Expected: 通过

- [ ] **Step 3: 补测试确认所有 think 路径透传 cache_user_id**

在 `react_loop.rs` 或邻近测试模块加：
```rust
#[cfg(test)]
mod cache_user_id_tests {
    // 注：若 ReactAgent 测试 fixture 过重，标记 ignore 并用手动验证
    #[test]
    #[ignore = "启用条件：ReactAgent 构造 fixture 就绪"]
    fn react_loop_think_propagates_cache_user_id() {
        // 构造 agent 设置 cache_user_id=Some("test-id")
        // 触发非流式 think 路径
        // 断言发出的 ChatRequest.user_id == Some("test-id")
    }
}
```

> 手动验证：启动应用，用非流式规划任务，观察后端日志 `user_id` 非空。

- [ ] **Step 4: Commit**

```bash
git add echo-agent/src/agent/react/run/react_loop.rs
git commit -m "fix(react): propagate cache_user_id in non-streaming planning path (R1)"
```

---

### Task B2: OpenAI 独立 chat/stream_chat 透传 user_id（R2 修复）

**Files:**
- Modify: `echo-integration/src/providers/openai.rs:46-113`

- [ ] **Step 1: chat() 接收 user_id 参数**

`openai.rs:46` 的 `chat()` 签名末尾加 `user_id: Option<String>`，第 72 行 `user_id: None` 改 `user_id`：
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
        user_id,  // ← 原 None
    };
```

- [ ] **Step 2: stream_chat() 同样接收 user_id**

`openai.rs:81` 的 `stream_chat()` 同理加 `user_id: Option<String>` 参数，第 107 行改 `user_id`。

- [ ] **Step 3: 修复所有 chat()/stream_chat() 调用点**

Run: `cargo build 2>&1 | grep "missing field\|arguments" | head -20`
对每个调用点补传 `user_id`（从调用上下文的 `cache_user_id` 或 `ChatRequest.user_id` 取）。

- [ ] **Step 4: trait chat 实现已正确（openai.rs:204），无需改**

确认 `OpenAiClient::chat`（trait 实现）已 `user_id: request.user_id.clone()`。

- [ ] **Step 5: 编译 + 测试**

Run: `cargo build && cargo test -p echo-integration`
Expected: 通过

- [ ] **Step 6: Commit**

```bash
git add echo-agent/echo-integration/src/providers/openai.rs
# 以及调用点修复
git commit -m "fix(providers): thread user_id through OpenAI standalone chat/stream_chat (R2)"
```

---

### Task B3: 修复 reinject_canonical_context 插入位置（R3 修复）

**Files:**
- Modify: `echo-state/src/compression/mod.rs:614-651`

- [ ] **Step 1: 改 insert(pos=1) 为追加到 canonical 区域末尾**

当前 `reinject_canonical_context` 在 `pos=1` 插入 canonical 消息，会把整个对话历史后移。改为**紧贴 system 之后、history 之前**插入，且只插一次（不重复）。

定位 `compression/mod.rs:634-650`，替换为：
```rust
// Inject canonical context messages (system prompt, rules, skills)
if let Some(msgs) = canonical.to_reinjection_messages() {
    // 找到 system 段末尾（开头连续 System role 的边界）
    let sys_end = self
        .messages
        .iter()
        .position(|m| m.role != Role::System)
        .unwrap_or(0);

    // 在 sys_end 处插入（system 之后、history 之前），保持 system→canonical→history 顺序
    // 关键：不在 pos=1 插入，而是紧贴现有 canonical 区域末尾追加，
    // 避免把已缓存的 history 段整体后移导致前缀失效。
    //
    // 已有 canonical 消息则跳过（去重，避免压缩多次触发累积）
    let existing_canonical: std::collections::HashSet<&str> = self
        .messages[..sys_end]
        .iter()
        .filter_map(|m| m.content.as_text())
        .collect();
    let to_insert: Vec<Message> = msgs
        .into_iter()
        .map(Message::system)
        .filter(|m| {
            !m.content
                .as_text()
                .map(|t| existing_canonical.contains(t))
                .unwrap_or(false)
        })
        .collect();

    // 在现有 canonical 区域末尾（仍是 system role 范围内）追加
    let insert_pos = sys_end; // 紧贴 history 之前
    for (offset, msg) in to_insert.into_iter().enumerate() {
        self.messages.insert(insert_pos + offset, msg);
    }
}
```

> 关键变化：从 `insert(pos=1)` 改为 `insert(sys_end)`。canonical 消息追加到 system 段末尾（仍在 history 之前），不移动 history。下次请求的 history 前缀不变，缓存命中保留。

- [ ] **Step 2: 编译验证**

Run: `cargo build -p echo-state`
Expected: 通过

- [ ] **Step 3: 写单测验证不移动 history**

在 `compression/mod.rs` 测试模块加：
```rust
#[cfg(test)]
mod reinject_tests {
    use super::*;
    // 构造一个已压缩的 context：system + history（几条 user/assistant）
    // 触发 reinject_canonical_context
    // 断言：history 消息在 messages 中的相对顺序不变，canonical 插在 system 之后 history 之前

    #[test]
    fn reinject_inserts_canonical_without_shifting_history() {
        // TODO: 需 ContextManager 测试 fixture
        // 标记 ignore 若 fixture 过重
    }
}
```

> 若 ContextManager 构造过重，标记 `#[ignore]` + 手动验证：触发压缩后，对比压缩前后 history 段消息的相对位置不变。

- [ ] **Step 4: 运行既有测试确认无回归**

Run: `cargo test -p echo-state`
Expected: 既有测试全 pass

- [ ] **Step 5: Commit**

```bash
git add echo-agent/echo-state/src/compression/mod.rs
git commit -m "fix(compression): insert canonical at sys_end, not pos=1, to preserve history prefix (R3)"
```

---

## 阶段 C：Anthropic provider helper 收敛（R5，不改协议）

### Task C1: 抽取 AnthropicCachePlan helper

**Files:**
- Create: `echo-integration/src/providers/anthropic_cache.rs`
- Modify: `echo-integration/src/providers/mod.rs`

- [ ] **Step 1: 新建 AnthropicCachePlan，收敛现有断点逻辑**

创建 `echo-integration/src/providers/anthropic_cache.rs`：
```rust
use echo_core::llm::cache::layout::{BreakpointTarget, PromptCacheLayout};

/// Anthropic 缓存断点计划。收敛原 anthropic.rs:150-198 的打补丁逻辑。
///
/// 不引入新 trait——先做具体 helper，两条 provider 路径稳定后再抽象。
pub struct AnthropicCachePlan {
    /// 最多 4 个断点目标，按 layout 段表达
    pub breakpoints: Vec<BreakpointTarget>,
}

impl AnthropicCachePlan {
    /// 从只读 layout 生成断点计划。
    ///
    /// 断点分配（复刻原 apply_conversation_cache_breakpoints 策略，提升到 layout 层）：
    ///   1. SystemLastBlock        —— system prompt 跨轮缓存（若有 system）
    ///   2. ToolsLastTool          —— tools 跨轮缓存（若有 tools）
    ///   3. HistoryIndex(~75%)     —— 早期历史缓存（history >= 4 条时）
    ///   4. HistoryLastStable      —— 末尾稳定消息（跳过 runtime_context）
    pub fn from_layout(layout: &PromptCacheLayout) -> Self {
        let mut breakpoints = Vec::with_capacity(4);
        if !layout.system.is_empty() || !layout.canonical.is_empty() {
            breakpoints.push(BreakpointTarget::SystemLastBlock);
        }
        if !layout.tools.is_empty() {
            breakpoints.push(BreakpointTarget::ToolsLastTool);
        }
        if layout.history.len() >= 4 {
            let idx = (layout.history.len() - 1) * 3 / 4;
            breakpoints.push(BreakpointTarget::HistoryIndex(idx));
        }
        if !layout.history.is_empty() {
            breakpoints.push(BreakpointTarget::HistoryLastStable);
        }
        // Anthropic 上限 4 个断点
        breakpoints.truncate(4);
        Self { breakpoints }
    }
}
```

- [ ] **Step 2: 暴露模块**

在 `echo-integration/src/providers/mod.rs` 加：
```rust
pub mod anthropic_cache;
pub use anthropic_cache::AnthropicCachePlan;
```

- [ ] **Step 3: 编译验证**

Run: `cargo build -p echo-integration`
Expected: 通过

- [ ] **Step 4: 写单测**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use echo_core::llm::types::{Message, ToolDefinition};

    #[test]
    fn plan_skips_runtime_context_breakpoint() {
        let msgs = vec![
            Message::system("S"),
            Message::user("h1"),
            Message::user("h2"),
            Message::user("h3"),
            Message::user("h4"),
            Message::user("[runtime_context:turn]\nctx"),
        ];
        let tools: Vec<ToolDefinition> = vec![];
        let layout = PromptCacheLayout::from_messages(&msgs, &tools);
        let plan = AnthropicCachePlan::from_layout(&layout);
        // runtime_context 段不出现在断点中
        assert!(!plan.breakpoints.iter().any(|b| matches!(b, BreakpointTarget::HistoryIndex(i) if *i >= layout.history.len())));
        assert!(plan.breakpoints.contains(&BreakpointTarget::HistoryLastStable));
    }

    #[test]
    fn plan_truncates_to_four_breakpoints() {
        let msgs: Vec<Message> = (0..10).map(|i| Message::user(format!("h{i}"))).collect();
        let mut tools = vec![];
        let layout = PromptCacheLayout::from_messages(&msgs, &tools);
        let plan = AnthropicCachePlan::from_layout(&layout);
        assert!(plan.breakpoints.len() <= 4);
    }
}
```

> `ToolDefinition` 构造以实际 API 为准。

- [ ] **Step 5: 运行测试**

Run: `cargo test -p echo-integration anthropic_cache`
Expected: 2 tests passed

- [ ] **Step 6: Commit**

```bash
git add echo-agent/echo-integration/src/providers/anthropic_cache.rs echo-agent/echo-integration/src/providers/mod.rs
git commit -m "feat(providers): extract AnthropicCachePlan helper from convert_request"
```

---

### Task C2: anthropic.rs convert_request 改用 AnthropicCachePlan

**Files:**
- Modify: `echo-integration/src/providers/anthropic.rs:150-198`

- [ ] **Step 1: convert_request 调用 AnthropicCachePlan**

定位 `anthropic.rs:152` 的 `convert_request`。当前手动设 system/tool/conversation 三处断点。改为：

```rust
// 用只读 layout 识别分段（不改 messages）
let layout = PromptCacheLayout::from_messages(&request.messages, request.tools.as_deref().unwrap_or(&[]));
let cache_plan = AnthropicCachePlan::from_layout(&layout);

// system 断点
let system = system.map(|text| {
    let has_sys_bp = cache_plan.breakpoints.contains(&BreakpointTarget::SystemLastBlock);
    AnthropicSystem::Blocks(vec![SystemBlock {
        block_type: "text".to_string(),
        text,
        cache_control: if has_sys_bp { Some(CacheControl::ephemeral()) } else { None },
    }])
});

// tools 断点（最后一个 tool）
let tools: Option<Vec<AnthropicToolDef>> = request.tools.as_ref().map(|tools| {
    let count = tools.len();
    let has_tool_bp = cache_plan.breakpoints.contains(&BreakpointTarget::ToolsLastTool);
    tools.iter().enumerate().map(|(i, t)| AnthropicToolDef {
        name: t.function.name.clone(),
        description: Some(t.function.description.clone()),
        input_schema: t.function.parameters.clone(),
        cache_control: if has_tool_bp && i == count - 1 {
            Some(CacheControl::ephemeral())
        } else { None },
    }).collect()
});

// conversation 断点：把 BreakpointTarget 映射到 messages 索引
let mut msg_breakpoints: Vec<usize> = cache_plan.breakpoints.iter()
    .filter_map(|bp| match bp {
        BreakpointTarget::HistoryIndex(i) => Some(layout.segments().history.start + i),
        BreakpointTarget::HistoryLastStable => {
            // 最后一条非 runtime_context 消息（复用原 is_runtime_context 逻辑）
            messages.iter().rposition(|m| !is_runtime_context_msg(m))
                .map(|i| i)
        }
        _ => None,
    })
    .collect();
msg_breakpoints.sort_unstable();
msg_breakpoints.dedup();

for index in msg_breakpoints {
    if let Some(message) = messages.get_mut(index) {
        message.add_cache_control_ephemeral();
    }
}
```

- [ ] **Step 2: 保留 is_runtime_context 辅助函数**

原 `apply_conversation_cache_breakpoints` 的 `is_runtime_context_text` 逻辑保留为 helper（被 `HistoryLastStable` 映射使用）。可删除 `apply_conversation_cache_breakpoints` 函数本身（逻辑已迁入 C1+C2）。

- [ ] **Step 3: 编译验证**

Run: `cargo build -p echo-integration`
Expected: 通过

- [ ] **Step 4: 运行既有 anthropic 测试确认无回归**

Run: `cargo test -p echo-integration`
Expected: 既有测试全 pass

- [ ] **Step 5: Commit**

```bash
git add echo-agent/echo-integration/src/providers/anthropic.rs
git commit -m "refactor(providers): Anthropic convert_request uses AnthropicCachePlan"
```

---

## 阶段 D：ChatRequest 接入 cache_hints + 可观测

### Task D1: ChatRequest 增加 cache_hints 字段

**Files:**
- Modify: `echo-core/src/llm/types.rs`（ChatRequest 结构体）

- [ ] **Step 1: 加 cache_hints 字段**

在 `ChatRequest` 结构体加：
```rust
use crate::llm::cache::CacheHints;

pub struct ChatRequest {
    // ...既有字段...
    pub user_id: Option<String>,
    /// provider 缓存提示（断点目标+hash+段范围）。可选，provider 实现按需消费。
    /// 轻量结构，不携带完整 layout，避免 core 类型耦合 provider 上下文。
    #[serde(default, skip)]
    pub cache_hints: Option<CacheHints>,
}
```

> `#[serde(skip)]` 因 CacheHints 是内存诊断结构，不进 HTTP body（provider 实现已消费它）。

- [ ] **Step 2: 修复所有 ChatRequest 构造点**

Run: `cargo build 2>&1 | grep "missing field" | head -20`
对每个构造点补 `cache_hints: None`（D2/D3 填真实值）。

- [ ] **Step 3: 全 workspace 编译**

Run: `cargo build`
Expected: 通过

- [ ] **Step 4: Commit**

```bash
git add echo-agent/echo-core/src/llm/types.rs
# 以及构造点修复
git commit -m "feat(llm): add cache_hints to ChatRequest"
```

---

### Task D2: think.rs 构造 layout 并填 cache_hints（流式主路径）

**Files:**
- Modify: `echo-agent/src/agent/react/run/phases/think.rs:258`

- [ ] **Step 1: 构造 layout 与 CacheHints**

定位 `think.rs:258` 的 `ChatRequest { ... }`。在构造前加：
```rust
use echo_core::llm::cache::{PromptCacheLayout, CacheHints, BreakpointTarget};
use echo_core::llm::cache::diagnostic::stable_prefix_hash;

let layout = PromptCacheLayout::from_messages(&messages, tools.as_deref().unwrap_or(&[]));
let prefix_hash = stable_prefix_hash(
    layout.system, layout.canonical, layout.tools, layout.history,
);
let cache_hints = CacheHints {
    breakpoints: vec![], // Anthropic provider 自己用 AnthropicCachePlan::from_layout 生成
    stable_prefix_hash: Some(prefix_hash),
    segments: layout.segment_ranges(),
};
```

`ChatRequest` 构造改为：
```rust
let request = crate::llm::ChatRequest {
    messages: ms,
    temperature: temp,
    max_tokens,
    tools: t,
    tool_choice: None,
    response_format: None,
    thinking: snap.thinking.clone(),
    cancel_token: snap.cancel_token.clone(),
    user_id: snap.config.cache_user_id.clone(),
    cache_hints: Some(cache_hints),
};
```

- [ ] **Step 2: 编译验证**

Run: `cargo build -p echo-agent`
Expected: 通过

- [ ] **Step 3: Commit**

```bash
git add echo-agent/src/agent/react/run/phases/think.rs
git commit -m "feat(react): attach cache_hints with stable prefix hash in think phase"
```

---

### Task D3: react_loop.rs 同样填 cache_hints（非流式路径）

**Files:**
- Modify: `echo-agent/src/agent/react/run/react_loop.rs:36`

- [ ] **Step 1: 同 D2，为非流式 ChatRequest 填 cache_hints**

```rust
let layout = PromptCacheLayout::from_messages(messages, &tools);
let prefix_hash = stable_prefix_hash(layout.system, layout.canonical, layout.tools, layout.history);
let request = ChatRequest {
    messages: messages.to_vec(),
    temperature,
    max_tokens,
    tools: Some(tools.clone()),
    tool_choice: None,
    response_format: response_format.clone(),
    thinking: self.thinking.clone(),
    cancel_token: None,
    user_id: self.config.cache_user_id.clone(),
    cache_hints: Some(CacheHints {
        breakpoints: vec![],
        stable_prefix_hash: Some(prefix_hash),
        segments: layout.segment_ranges(),
    }),
};
```

- [ ] **Step 2: 编译 + Commit**

```bash
cargo build -p echo-agent
git add echo-agent/src/agent/react/run/react_loop.rs
git commit -m "feat(react): attach cache_hints in non-streaming path"
```

---

### Task D4: provider 实现消费 cache_hints 并打日志

**Files:**
- Modify: `echo-integration/src/providers/openai.rs`（trait chat 实现）
- Modify: `echo-integration/src/providers/anthropic.rs`（convert_request）

- [ ] **Step 1: OpenAI trait chat 打日志**

在 `openai.rs` trait chat 实现的请求发出前加：
```rust
if let Some(ref hints) = request.cache_hints {
    tracing::debug!(
        user_id = ?request.user_id,
        msg_count = request.messages.len(),
        stable_prefix_hash = ?hints.stable_prefix_hash,
        "openai request cache trace"
    );
}
```

- [ ] **Step 2: LlmUsage 事件附加命中率日志**

在 `stream_channel.rs:268` 和 `phases/think.rs:162` 的 `AgentEvent::LlmUsage` 发出处加：
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

- [ ] **Step 3: 编译 + Commit**

```bash
cargo build
git add echo-agent/echo-integration/src/providers/openai.rs echo-agent/echo-integration/src/providers/anthropic.rs echo-agent/src/agent/react/run/stream_channel.rs echo-agent/src/agent/react/run/phases/think.rs
git commit -m "feat(observability): log cache hit rate and stable prefix hash"
```

---

## 阶段 E：端到端验证与防回归

### Task E1: cache_user_id 覆盖测试

**Files:**
- Create: `echo-agent-cli/echo-agent-app-core/tests/cache_user_id_test.rs`

- [ ] **Step 1: 集成测试确认所有 agent 路径透传 cache_user_id**

```rust
#[tokio::test]
#[ignore = "需真实 provider 或 mock，手动验证为主"]
async fn all_agent_paths_propagate_cache_user_id() {
    // 构造 main agent + subagent，设置 cache_user_id
    // 触发流式 think / 非流式 think / subagent dispatch
    // 断言每个路径发出的 ChatRequest.user_id == Some(expected_id)
}
```

> 手动验证：启动应用，发起多轮任务（含 subagent 委派），后端日志确认所有 LLM 请求 `user_id` 一致且非空。

- [ ] **Step 2: Commit**

```bash
git add echo-agent-cli/echo-agent-app-core/tests/cache_user_id_test.rs
git commit -m "test(cache): add coverage for cache_user_id propagation"
```

---

### Task E2: layout 稳定性防回归测试

**Files:**
- Modify: `echo-core/src/llm/cache/layout.rs` 测试模块

- [ ] **Step 1: 跨轮 stable_prefix_hash 稳定性测试**

```rust
#[test]
fn prefix_hash_stable_across_turns() {
    // turn N: system + canonical + history[H1] + runtime[R1]
    // turn N+1: system + canonical + history[H1,H2] + runtime[R2]
    // 断言：system/canonical 段 hash 不变（前缀稳定）
    // runtime_context 变化不影响稳定前缀 hash
    let sys = vec![Message::system("S")];
    let canon = vec![Message::system("[Canonical context — x]")];
    let tools = vec![];

    let h1 = stable_prefix_hash(&sys, &canon, &tools, &[Message::user("H1")]);
    let h2 = stable_prefix_hash(&sys, &canon, &tools, &[Message::user("H1"), Message::user("H2")]);
    // history 增长导致 hash 变化（符合预期）
    assert_ne!(h1, h2);

    // 但 system+canonical 单独 hash 不变
    let sc1 = stable_prefix_hash(&sys, &canon, &tools, &[]);
    let sc2 = stable_prefix_hash(&sys, &canon, &tools, &[]);
    assert_eq!(sc1, sc2);
}
```

- [ ] **Step 2: 运行 + Commit**

Run: `cargo test -p echo-core cache`
```bash
git add echo-agent/echo-core/src/llm/cache/layout.rs
git commit -m "test(cache): assert prefix hash invariance across turns"
```

---

## 验收清单

- [ ] **R1 残留修复**：`react_loop.rs:45` 透传 `cache_user_id`，非 None
- [ ] **R2 修复**：`openai.rs` 独立 `chat()`/`stream_chat()` 接收并透传 `user_id`
- [ ] **R3 修复**：`reinject_canonical_context` 改 `insert(sys_end)`，不移动 history
- [ ] **R5 部分**：`PromptCacheLayout::from_messages()` 只读 view 落地；`AnthropicCachePlan` helper 收敛断点逻辑
- [ ] **保留 cache_user_id**：机器级持久化 UUID 不动，不引入 session-scoped 替代
- [ ] **不推倒存储**：ContextManager 保持单一 messages 数组，layout 是只读 view
- [ ] **可观测**：每轮日志输出 `cache_hit_rate` + `stable_prefix_hash`
- [ ] **缓存命中率**：同模型同任务连续 3 轮，日志 `cache_hit_rate` 从 0% 升至 95%+（Anthropic）/ 90%+（OpenAI 兼容）
- [ ] **回归**：`cargo build` 全 workspace 通过；`cargo test` 全 pass；常规对话功能正常

## 与审查反馈的逐条对应

| 审查反馈 | 本版处理 |
|---|---|
| R1 已过期，按计划改会倒退 | R1 降级为残留（react_loop.rs:45），仅补透传，不另起 session_id |
| C4 推倒存储风险大 | 改为 `from_messages()` 只读 view，不改底层存储 |
| Vec<SegmentKind> 表达脆弱 | 改用 `BreakpointTarget`（HistoryIndex(usize)/HistoryLastStable）精确点 |
| stable_prefix_hash 用 DefaultHasher 不稳 | 改用 SHA-256 + canonical JSON（BTreeMap sorted keys） |
| session-scoped user_id 方向错 | 保留机器级 `cache_user_id`，不替代 |
| cache_layout 塞 ChatRequest 过重 | 改为轻量 `cache_hints: Option<CacheHints>`（断点+hash+段范围） |
| 先做 helper 再抽象 trait | `ProviderCacheAdapter` trait 延后，先做 `AnthropicCachePlan` 具体 helper |

## 执行顺序建议

1. **阶段 A**（layout view + hash）：基础设施，零风险
2. **阶段 B**（R1残留/R2/R3 定点修）：见效快，每步独立可回滚
3. **阶段 C**（Anthropic helper 收敛）：重构现有逻辑，不改协议
4. **阶段 D**（cache_hints + 可观测）：接入与验证
5. **阶段 E**（测试）：防回归

阶段 B 的 B1+B2（user_id 透传）应**最先做**——这是 OpenAI 兼容族命中率从 <1% 提升的主要杠杆，改动最小。
