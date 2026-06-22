# Provider 架构决策

> **状态**：已采纳（2026-06）
> **适用范围**：echo-agent / echo-integration / echo-agent-cli 全栈
> **决策性质**：架构约束——偏离此决策需显式评审

## 决策陈述

**LLM provider 层只维护两个基础实现：OpenAI Chat Completions + Anthropic Messages。**
所有其他厂商通过 `LlmConfig` 预设接入对应基础协议，个性化差异收敛在 usage 解析与缓存适配器。**不实现 OpenAI Responses API**，不为每个国内厂商建独立 provider 文件。

## 决策理由

### 1. Chat Completions 是行业事实标准，覆盖面足够

OpenAI Chat Completions API 是当前覆盖面最广的"通行证"。DeepSeek、Kimi(Moonshot)、通义千问、智谱 GLM、xAI、Groq、Mistral 等均兼容此协议。维护一个高质量的 Chat Completions 实现，即可覆盖绝大多数主流厂商。

### 2. Anthropic Messages API 协议差异大，必须独立实现

Anthropic 的 `cache_control` 断点、`system` 独立字段、content blocks 结构与 Chat Completions 差异显著，强行统一会增加复杂度且削弱 Anthropic 的 prompt cache 能力。独立实现是合理的。

### 3. 不实现 Responses API——与 prompt cache 架构冲突

OpenAI Responses API（2025 年推出）把对话状态移到服务端，客户端只发 `previous_response_id`。这与本项目的 prompt cache 优化目标**根本冲突**：

- `PromptCacheLayout` / `cache_hints` / 稳定 prefix 依赖客户端控制完整 messages 数组
- Responses 模式下 prefix 在服务端，客户端无法保证其字节稳定
- `cache_user_id` 的 KV-cache 分区机制在 Responses 模式下语义不同

Chat Completions 已能覆盖 OpenAI 自家模型，Responses API 投入产出比低且会削弱缓存效果，**不实现**。

### 4. 不为每个国内厂商建独立 provider

国内厂商的"兼容 OpenAI"是有条件的（usage 字段名差异、非标准扩展参数），但这些差异通过 `LlmConfig` 预设 + usage 解析 fallback + 缓存适配器就能处理，**不需要独立 provider 实现**。为每个厂商建文件会导致维护成本倍增且重复代码。

## 当前架构

### 文件结构

```
echo-integration/src/providers/
├── openai.rs            ← OpenAI Chat Completions 基础实现（覆盖 OpenAI/DeepSeek/GLM/Kimi/Qwen 等）
│   ├── chat()           ← 独立函数（非流式，接收 user_id）
│   ├── stream_chat()    ← 独立函数（流式，接收 user_id）
│   └── impl LlmClient   ← trait 实现（转发 request.user_id）
├── openai_cache.rs      ← OpenAI 缓存适配器（稳定 user_id + 前缀缓存）
├── anthropic.rs         ← Anthropic Messages API 独立实现
├── anthropic_cache.rs   ← Anthropic 缓存适配器（cache_control 断点策略）
└── config.rs            ← LlmConfig 预设（openai/anthropic/deepseek 等）
```

### 个性化差异的处理位置

| 差异类型 | 处理位置 | 示例 |
|---|---|---|
| base_url / model name | `LlmConfig` 预设（`config.rs`） | `LlmConfig::deepseek()` 设 base_url=deepseek 官方 |
| usage 字段名差异 | `Usage::cached_prompt_tokens()`（`types.rs:690-703`） | 按优先级 fallback：OpenAI `cached_tokens` → Anthropic `cache_read_input_tokens` → DeepSeek `prompt_cache_hit_tokens` |
| 缓存行为 | `OpenAiCacheAdapter` / `AnthropicCachePlan` | OpenAI 靠稳定 prefix + user_id；Anthropic 靠 cache_control 断点 |
| 厂商专属参数 | `ChatRequest.glm_thinking` 字段（**待改进**） | GLM `enable_thinking` |

### 两个基础 provider 的协议映射

**OpenAI Chat Completions（`OpenAiClient`）**：
- 端点：`POST /v1/chat/completions`
- 状态：无状态，客户端持有完整 history
- 缓存：自动前缀缓存（依赖稳定 prefix + 稳定 `user` 字段）
- 覆盖：OpenAI / DeepSeek / GLM / Kimi / Qwen / xAI / Groq / Mistral 等

**Anthropic Messages（`AnthropicClient`）**：
- 端点：`POST /v1/messages`
- 状态：无状态，客户端持有完整 history
- 缓存：显式 `cache_control: ephemeral` 断点（最多 4 个，由 `AnthropicCachePlan` 分配）
- 覆盖：Anthropic Claude 系列（DeepSeek 也兼容此协议，但走 OpenAI 路径即可）

## 厂商接入指南

新增一个厂商时，**不需要写新的 provider 代码**。步骤：

### 1. 添加 `LlmConfig` 预设（如需要）

在 `echo-integration/src/providers/config.rs` 加一个构造方法：

```rust
pub fn new_provider(api_key: impl Into<String>, model: impl Into<String>) -> Self {
    Self::new(api_key, model)
        .with_base_url("https://api.newprovider.com/v1")
        // 如有非标准行为，在此设置
}
```

如果新厂商的 base_url 和 model 命名规则与 OpenAI 一致，连预设都不需要——用户直接用 `LlmConfig::new()` 配置即可。

### 2. 确认 usage 字段兼容性

新厂商的 `cached_tokens` 字段位置如果与 OpenAI 标准（`prompt_tokens_details.cached_tokens`）不同，在 `Usage::cached_prompt_tokens()`（`echo-core/src/llm/types.rs:690`）的 fallback 链中补一个分支。**不要**在 provider 文件里特殊处理。

### 3. 确认缓存行为

走 OpenAI 兼容路径的厂商，`OpenAiCacheAdapter` 已经处理了稳定 `user_id` + 前缀缓存。如果该厂商的缓存机制有特殊要求（如需要额外的 header），在 `OpenAiCacheAdapter` 或 `OpenAiClient` 中补充，**不要建新 provider**。

### 4. 厂商专属参数（如需要）

如果厂商有非标准扩展参数（如 GLM 的 `enable_thinking`）：

- **当前做法**（可接受）：在 `ChatRequest` 加专属字段，`OpenAiClient` 序列化时处理
- **未来改进**（推荐）：收敛到 `provider_extensions: HashMap<String, serde_json::Value>`，避免核心请求类型耦合单厂商

## 反模式（不要做）

### ❌ 为每个国内厂商建独立 provider 文件

```rust
// 不要这样做
echo-integration/src/providers/deepseek.rs   // ❌
echo-integration/src/providers/glm.rs        // ❌
echo-integration/src/providers/kimi.rs       // ❌
```

这些厂商都是 OpenAI Chat Completions 协议，差异通过 `LlmConfig` + usage fallback 处理。建独立文件会导致 90% 代码重复。

### ❌ 实现 OpenAI Responses API

```rust
// 不要这样做
echo-integration/src/providers/responses.rs  // ❌
```

Responses API 的服务端状态管理与本项目的 prompt cache 架构（`PromptCacheLayout` / `cache_hints` / 稳定 prefix）冲突。Chat Completions 已覆盖 OpenAI 自家模型。

### ❌ 在核心类型加厂商专属字段

```rust
// 不要这样做（当前 glm_thinking 是历史遗留，可接受但不应扩展）
pub struct ChatRequest {
    pub glm_thinking: Option<GlmThinkingBlock>,      // ❌ 厂商耦合
    pub deepseek_reasoning: Option<bool>,             // ❌
    pub qwen_enable_search: Option<bool>,             // ❌
}
```

未来新增厂商专属参数应走 `provider_extensions` map。

### ❌ 统一 OpenAI 和 Anthropic 两个基础实现

不要为了"统一"而抽象出泛型 provider 接口把两者合并。两者的协议差异（system 字段位置、content blocks、cache_control）是本质性的，强行统一会增加复杂度且削弱各自的缓存能力。

## 已确认覆盖的厂商

| 厂商 | 接入方式 | 缓存支持 | 备注 |
|---|---|---|---|
| OpenAI | `LlmConfig::openai()` + `OpenAiClient` | ✅ `cached_tokens` | 基础实现 |
| DeepSeek | `LlmConfig::deepseek()` + `OpenAiClient` | ✅ `prompt_cache_hit_tokens` | usage 字段已 fallback 处理 |
| Anthropic Claude | `LlmConfig::anthropic()` + `AnthropicClient` | ✅ `cache_control` 断点 | 独立实现 |
| 智谱 GLM | `LlmConfig::new()` + `OpenAiClient` | ✅ OpenAI 兼容路径 | `glm_thinking` 字段处理扩展参数 |
| Kimi (Moonshot) | `LlmConfig::new()` + `OpenAiClient` | ✅ OpenAI 兼容路径 | |
| 通义千问 (Qwen) | `LlmConfig::new()` + `OpenAiClient` | ✅ OpenAI 兼容路径 | |
| xAI / Groq / Mistral 等 | `LlmConfig::new()` + `OpenAiClient` | ✅ OpenAI 兼容路径 | |

## 相关代码索引

- `echo-integration/src/providers/config.rs` — `LlmConfig` 预设
- `echo-integration/src/providers/openai.rs` — OpenAI Chat Completions 实现
- `echo-integration/src/providers/openai_cache.rs` — OpenAI 缓存适配器
- `echo-integration/src/providers/anthropic.rs` — Anthropic Messages 实现
- `echo-integration/src/providers/anthropic_cache.rs` — Anthropic 缓存适配器
- `echo-core/src/llm/types.rs:566` — `ChatRequest`（含 `glm_thinking` 字段，待改进）
- `echo-core/src/llm/types.rs:690-703` — `Usage::cached_prompt_tokens()` 多厂商 fallback
- `echo-core/src/llm/cache/layout.rs` — `PromptCacheLayout`（与 Responses API 冲突的根因）
- `echo-agent-app-core/src/infra.rs:69` — `load_or_create_cache_user_id()`（机器级稳定 ID）

## 决策回顾触发条件

如遇以下情况，需重新评审本决策：

1. 某主流厂商**仅**支持 Responses API 且无法通过 Chat Completions 接入
2. Responses API 引入客户端可控的 prefix 缓存机制（消除与 prompt cache 架构的冲突）
3. 某厂商的协议差异大到无法通过 `LlmConfig` + usage fallback 处理（需评估独立 provider）
4. `glm_thinking` 模式的厂商专属字段超过 3 个（需收敛到 `provider_extensions`）
