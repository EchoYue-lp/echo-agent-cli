# Provider 收敛实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 从 9 个 LlmClient 实现收敛到 2 个（OpenAi + Anthropic），删除 7 个冗余/不支持的 provider 文件，同步更新 config 枚举/metadata/build_client + 应用层 infra.rs + 架构文档。

**Architecture:** 4 个国内厂商 client（DeepSeek/Qwen/Glm/Kimi）是纯冗余——OpenAiClient 已用 `config.provider_name` 调 `translate_thinking_openai_compat` 正确处理它们。Gemini/Azure/Ollama 暂不支持，直接删除。GUI 从后端动态获取 provider 列表，后端删 metadata 条目后 GUI 自动不显示。

**Tech Stack:** Rust / serde（echo-agent + echo-integration + echo-agent-cli）。

---

## 文件结构（改动总览）

**删除（7 个文件）：**
- `echo-integration/src/providers/deepseek.rs`
- `echo-integration/src/providers/qwen.rs`
- `echo-integration/src/providers/glm.rs`
- `echo-integration/src/providers/kimi.rs`
- `echo-integration/src/providers/gemini.rs`
- `echo-integration/src/providers/azure.rs`
- `echo-integration/src/providers/ollama.rs`

**修改（框架层）：**
- `echo-integration/src/providers/mod.rs` — 删模块声明 + re-export
- `echo-integration/src/providers/config.rs` — LlmProvider 枚举 / BUILTIN_PROVIDER_METADATA / 预设方法 / build_client / provider_urls / parse_provider / detect_provider_from_url / provider_base_url / provider_env_var_names

**修改（应用层）：**
- `echo-agent-app-core/src/infra.rs` — build_llm_config + provider_required_keys

**修改（文档）：**
- `docs/architecture/providers.md` — 更新为 2 client 现实

---

## Task 1: 删除 7 个 provider 文件

**Files:**
- Delete: `echo-integration/src/providers/{deepseek,qwen,glm,kimi,gemini,azure,ollama}.rs`

- [ ] **Step 1: 删除 7 个文件**

```bash
cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent/echo-agent
git rm echo-integration/src/providers/deepseek.rs \
       echo-integration/src/providers/qwen.rs \
       echo-integration/src/providers/glm.rs \
       echo-integration/src/providers/kimi.rs \
       echo-integration/src/providers/gemini.rs \
       echo-integration/src/providers/azure.rs \
       echo-integration/src/providers/ollama.rs
```

- [ ] **Step 2: 暂不编译（mod.rs 和 config.rs 还引用这些模块，编译必然失败，Task 2/3 修复后再验证）**

- [ ] **Step 3: Commit**

```bash
git commit --no-gpg-sign -m "refactor(providers): delete 7 redundant/unsupported vendor client files

DeepSeek/Qwen/Glm/Kimi are pure redundancy (OpenAiClient handles them
via translate_thinking_openai_compat + config.provider_name).
Gemini/Azure/Ollama暂不支持. Will update mod.rs/config.rs next."
```

---

## Task 2: 更新 mod.rs 删除模块声明与 re-export

**Files:**
- Modify: `echo-integration/src/providers/mod.rs`

- [ ] **Step 1: 删除 7 个 pub mod 声明**

删除以下行：
```rust
pub mod azure;
pub mod deepseek;
pub mod gemini;
pub mod glm;
pub mod kimi;
pub mod ollama;
pub mod qwen;
```

保留：`adapter_client` / `anthropic` / `anthropic_cache` / `client` / `config` / `openai` / `openai_cache` / `thinking_translate` / `traits`。

- [ ] **Step 2: 删除对应的 pub use re-export**

删除：
```rust
pub use azure::AzureOpenAiClient;
pub use deepseek::DeepSeekClient;
pub use gemini::GeminiClient;
pub use glm::GlmClient;
pub use kimi::KimiClient;
```

以及 ollama/qwen 的 re-export（若存在）。保留 `AnthropicClient` / `OpenAiClient` / `AdapterClient` / `AnthropicCachePlan` / config 类型的 re-export。

- [ ] **Step 3: 检查是否有 traits.rs 或其他文件引用被删类型**

Run: `cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent/echo-agent && grep -rn "DeepSeekClient\|QwenClient\|GlmClient\|KimiClient\|GeminiClient\|AzureOpenAiClient\|OllamaClient" echo-integration/src/ --include="*.rs"`
Expected: 无匹配（之前已确认外部无引用，mod.rs 删除后应干净）

- [ ] **Step 4: Commit**

```bash
git add echo-integration/src/providers/mod.rs
git commit --no-gpg-sign -m "refactor(providers): remove deleted vendor modules from mod.rs"
```

---

## Task 3: 更新 config.rs 的 LlmProvider 枚举与 BUILTIN_PROVIDER_METADATA

**Files:**
- Modify: `echo-integration/src/providers/config.rs:146-158`（LlmProvider 枚举）
- Modify: `echo-integration/src/providers/config.rs:58-123`（BUILTIN_PROVIDER_METADATA）

- [ ] **Step 1: LlmProvider 枚举只留 OpenAi + Anthropic**

将 `LlmProvider` 枚举（146 行）改为：
```rust
pub enum LlmProvider {
    /// OpenAI 兼容 API（默认，适用于 OpenAI、DashScope、DeepSeek、Moonshot、智谱 等）
    #[default]
    OpenAi,
    /// Anthropic Messages API
    Anthropic,
}
```

删除 `Ollama` / `Gemini` / `Azure` 三个变体。

- [ ] **Step 2: BUILTIN_PROVIDER_METADATA 删 gemini 和 ollama 条目**

删除 `BUILTIN_PROVIDER_METADATA` 数组中的两个 `ProviderMetadata`：
```rust
ProviderMetadata {
    id: "gemini",
    name: "Gemini",
    ...
},
ProviderMetadata {
    id: "ollama",
    name: "Ollama",
    ...
},
```

保留 6 个：deepseek / dashscope / openai / anthropic / moonshot / zhipu。

- [ ] **Step 3: 删除 provider_urls 的 GEMINI 和 OLLAMA 常量**

删除（34-42 行区域）：
```rust
pub const OLLAMA: &str = "http://localhost:11434/api/chat";
pub const GEMINI: &str = "https://generativelanguage.googleapis.com/v1beta/openai/";
```

保留 OPENAI / ANTHROPIC / DEEPSEEK / DASHSCOPE / MOONSHOT / ZHIPU。注意：BUILTIN_PROVIDER_METADATA 里的 `base_url: provider_urls::GEMINI` 已随 gemini 条目删除，不会残留引用。

- [ ] **Step 4: 暂不编译（build_client/parse_provider 等仍引用删掉的枚举变体，Task 4 修复）**

- [ ] **Step 5: Commit**

```bash
git add echo-integration/src/providers/config.rs
git commit --no-gpg-sign -m "refactor(providers): trim LlmProvider enum and metadata to OpenAi+Anthropic"
```

---

## Task 4: 更新 build_client / parse_provider / detect_provider_from_url / 辅助函数

**Files:**
- Modify: `echo-integration/src/providers/config.rs:372-441`（build_client）
- Modify: `echo-integration/src/providers/config.rs:613-622`（parse_provider）
- Modify: `echo-integration/src/providers/config.rs:624+`（detect_provider_from_url）
- Modify: `echo-integration/src/providers/config.rs:596-610`（provider_base_url）
- Modify: `echo-integration/src/providers/config.rs:1352-1365`（provider_env_var_names）
- Modify: `echo-integration/src/providers/config.rs:297/333/347`（删 ollama/gemini/azure 预设方法）

- [ ] **Step 1: 简化 build_client——OpenAi 分支统一走 OpenAiClient**

将 `build_client()`（372 行）改为：
```rust
pub fn build_client(&self) -> Result<Box<dyn echo_core::llm::LlmClient>> {
    match self.provider {
        LlmProvider::OpenAi => {
            let client = super::openai::OpenAiClient::new(self.clone())?;
            Ok(Box::new(client))
        }
        LlmProvider::Anthropic => {
            let client = super::anthropic::AnthropicClient::with_base_url(
                &self.base_url,
                &self.api_key,
                &self.model,
            );
            Ok(Box::new(client))
        }
    }
}
```

删除原来的 provider_name match（deepseek/qwen/glm/kimi 分发）和 Ollama/Gemini/Azure 三个分支。

- [ ] **Step 2: 简化 parse_provider——只剩 Anthropic，其余 OpenAi**

将 `parse_provider`（613 行）改为：
```rust
fn parse_provider(provider: &str) -> LlmProvider {
    match provider.to_lowercase().as_str() {
        "anthropic" => LlmProvider::Anthropic,
        // OpenAI 兼容类（openai、deepseek、dashscope、moonshot、zhipu 等）统一走 OpenAI 实现
        _ => LlmProvider::OpenAi,
    }
}
```

- [ ] **Step 3: 更新 detect_provider_from_url——移除 gemini/ollama/azure URL 检测**

定位 `detect_provider_from_url`（624 行附近），删除匹配 `ollama`/`gemini`/`azure` URL 的分支，只保留 `anthropic` 检测，其余兜底 `LlmProvider::OpenAi`。

- [ ] **Step 4: 更新 provider_base_url——删 ollama/gemini 分支**

将 `provider_base_url`（596 行）的 match 改为：
```rust
pub fn provider_base_url(provider: &str) -> Option<&'static str> {
    match provider.to_lowercase().as_str() {
        "openai" => Some("https://api.openai.com/v1/chat/completions"),
        "anthropic" => Some("https://api.anthropic.com/v1/messages"),
        "deepseek" => Some("https://api.deepseek.com/chat/completions"),
        "dashscope" | "qwen" | "aliyun" => {
            Some("https://dashscope.aliyuncs.com/compatible-mode/v1/chat/completions")
        }
        "moonshot" | "kimi" => Some("https://api.moonshot.cn/v1/chat/completions"),
        "zhipu" | "glm" => Some("https://open.bigmodel.cn/api/paas/v4/chat/completions"),
        _ => None,
    }
}
```

删除 `ollama` 和 `gemini | google` 分支。

- [ ] **Step 5: 更新 provider_env_var_names——删 gemini/azure/ollama 分支**

将 `provider_env_var_names`（1352 行）改为：
```rust
pub fn provider_env_var_names(provider: &str) -> &'static [&'static str] {
    match provider.to_lowercase().as_str() {
        "anthropic" => &["ANTHROPIC_API_KEY"],
        "openai" => &["OPENAI_API_KEY"],
        "deepseek" => &["DEEPSEEK_API_KEY"],
        "dashscope" | "qwen" | "aliyun" => &["DASHSCOPE_API_KEY", "QWEN_API_KEY"],
        "moonshot" | "kimi" => &["MOONSHOT_API_KEY", "KIMI_API_KEY"],
        "zhipu" | "glm" => &["ZHIPU_API_KEY", "GLM_API_KEY"],
        _ => &[],
    }
}
```

删除 `gemini | google` 和 `azure | azure_openai` 和 `ollama` 分支。

- [ ] **Step 6: 删除 LlmConfig::ollama / gemini / azure 预设方法**

删除以下三个方法（297 / 333 / 347 行附近）：
- `pub fn ollama(model: impl Into<String>) -> Self`
- `pub fn gemini(api_key: impl Into<String>, model: impl Into<String>) -> Self`
- `pub fn azure(...) -> Self`

保留 `openai()` / `anthropic()` / `deepseek()` / `dashscope()`。

- [ ] **Step 7: 编译验证 echo-integration**

Run: `cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent/echo-agent && cargo build -p echo-integration 2>&1 | tail -10`
Expected: 编译通过。若有错误（如其他地方引用了删掉的枚举变体/方法），逐一修复。

- [ ] **Step 8: 编译验证全 workspace**

Run: `cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent/echo-agent && cargo build --features subagent 2>&1 | tail -10`
Expected: 编译通过

- [ ] **Step 9: Commit**

```bash
git add echo-integration/src/providers/config.rs
# 以及 Step 7/8 修复的其他文件
git commit --no-gpg-sign -m "refactor(providers): simplify build_client/parse_provider/urls to OpenAi+Anthropic only"
```

---

## Task 5: 更新应用层 infra.rs

**Files:**
- Modify: `echo-agent-app-core/src/infra.rs:1191-1202`（build_llm_config）
- Modify: `echo-agent-app-core/src/infra.rs:934-945`（provider_required_keys）

- [ ] **Step 1: build_llm_config 删 gemini/ollama 分支 + 兜底加 warn**

将 `build_llm_config`（1191 行）的 match 改为：
```rust
let mut config = match provider.to_lowercase().as_str() {
    "anthropic" => LlmConfig::anthropic(auth_token, model),
    "deepseek" => LlmConfig::deepseek(auth_token, model),
    "dashscope" | "qwen" | "aliyun" => LlmConfig::dashscope(auth_token, model),
    _ => {
        // 兜底：按 OpenAI 兼容处理。gemini/azure/ollama 等暂不支持的
        // provider 会落到这里，其 auth 差异可能导致请求失败。
        if matches!(
            provider.to_lowercase().as_str(),
            "gemini" | "google" | "ollama" | "azure" | "azure_openai"
        ) {
            tracing::warn!(
                provider = %provider,
                "provider 暂不支持，按 OpenAI 兼容处理（auth 差异可能导致失败）"
            );
        }
        let url = base_url_override.unwrap_or(default_base_url);
        LlmConfig::new(url, auth_token, model)
    }
};
```

删除 `"gemini" | "google"` 和 `"ollama"` 两个显式分支。

- [ ] **Step 2: provider_required_keys 删 ollama 分支**

将 `provider_required_keys`（934 行）的 match 中删除 `"ollama" => &[],` 行。其余保留（gemini/azure 本就不在这个函数的显式分支里，走兜底 `&[]`）。

- [ ] **Step 3: 编译验证应用层**

Run: `cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent/echo-agent-cli && cargo build -p echo-agent-app-core 2>&1 | tail -8`
Expected: 编译通过

- [ ] **Step 4: 编译验证 GUI（含 tauri commands）**

Run: `cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent/echo-agent-cli && cargo build --features gui 2>&1 | tail -8`
Expected: 编译通过

- [ ] **Step 5: Commit**

```bash
cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent/echo-agent-cli
git add echo-agent-app-core/src/infra.rs
git commit --no-gpg-sign -m "refactor(infra): drop gemini/ollama branches, warn on unsupported providers"
```

---

## Task 6: 冒烟测试验证厂商 thinking 协议

**Files:**
- Reuse: `echo-agent/examples/smoke_usage_passthrough.rs`（阶段 A 创建，验证 DeepSeek delegate）

- [ ] **Step 1: 跑既有 smoke test 确认 OpenAiClient 仍正确处理 DeepSeek**

Run: `cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent/echo-agent && cargo run --example smoke_usage_passthrough --features "subagent,tasks" --release 2>&1 | tail -20`
Expected: 冒烟通过——`SubagentResult.usage` 非 None，tokens > 0，model 非 unknown。这验证删除 DeepSeekClient 后 OpenAiClient 仍正确处理 DeepSeek（thinking + usage）。

- [ ] **Step 2: 若冒烟失败，排查 thinking 字段填充**

若 Step 1 失败（如 thinking 协议不对导致响应异常），检查：
- `OpenAiClient` 的 `build_request` 是否正确读取 `config.provider_name`
- `LlmConfig::deepseek()` 是否设置了 `provider_name: Some("deepseek")`
- `translate_thinking_openai_compat("deepseek", ...)` 是否返回正确字段

- [ ] **Step 3: 手动验证 Qwen/GLM/Kimi（如有 API key）**

若环境配置了其他厂商 key，用对应 model 跑 smoke test。若只有 DeepSeek key，Step 1 通过即足够（DeepSeek 验证了 OpenAiClient 的 provider_name 分发机制）。

- [ ] **Step 4: Commit（若有 smoke test 适配改动）**

```bash
git add examples/smoke_usage_passthrough.rs 2>/dev/null
git commit --no-gpg-sign -m "test(providers): verify OpenAiClient handles DeepSeek after vendor client removal" 2>/dev/null || echo "无改动，跳过"
```

---

## Task 7: 运行全量测试 + 更新架构文档

**Files:**
- Modify: `docs/architecture/providers.md`

- [ ] **Step 1: 全量测试回归**

Run: `cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent/echo-agent && cargo test --features subagent 2>&1 | tail -5`
Expected: 既有测试全 pass。若有测试引用了删掉的 client 类型/枚举变体，修复或删除该测试。

Run: `cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent/echo-agent-cli && cargo test --features gui 2>&1 | tail -5`
Expected: 全 pass

- [ ] **Step 2: 更新架构文档 providers.md**

更新 `docs/architecture/providers.md`，改为反映"2 个 client"现实：
- 决策陈述：改为"2 个 LlmClient 实现（OpenAi + Anthropic）"
- 文件结构：删除 deepseek/qwen/glm/kimi/gemini/azure/ollama 行
- 删除"反模式：不为厂商建独立文件"那节（因为现在确实没有了）
- 新增"暂不支持 Gemini/Azure/Ollama"说明
- 厂商接入指南：保留 LlmConfig 预设流程，但说明 thinking 差异由 `translate_thinking_openai_compat` 统一处理
- 更新"已确认覆盖厂商表"：移除 Gemini/Ollama/Azure 行

- [ ] **Step 3: Commit 文档**

```bash
cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent/echo-agent-cli
git add docs/architecture/providers.md
git commit --no-gpg-sign -m "docs(architecture): update providers.md to reflect 2-client reality"
```

---

## 验收清单

- [ ] 7 个 provider 文件已删除（deepseek/qwen/glm/kimi/gemini/azure/ollama）
- [ ] `LlmProvider` 枚举只剩 `OpenAi` + `Anthropic`
- [ ] `BUILTIN_PROVIDER_METADATA` 只剩 6 个 provider（deepseek/dashscope/openai/anthropic/moonshot/zhipu）
- [ ] `build_client()` OpenAi 分支统一走 `OpenAiClient::new`，无 provider_name 分发
- [ ] `LlmConfig::ollama()`/`gemini()`/`azure()` 预设方法已删
- [ ] `provider_base_url`/`provider_env_var_names`/`parse_provider`/`detect_provider_from_url` 无 gemini/ollama/azure 分支
- [ ] `infra.rs` 的 `build_llm_config` 无 gemini/ollama 分支，兜底有 warn
- [ ] GUI "模型服务商"页面显示 6 个 provider + "自定义"（动态获取，前端零改动）
- [ ] `cargo build --features subagent`（echo-agent）通过
- [ ] `cargo build --features gui`（echo-agent-cli）通过
- [ ] `cargo test`（两个仓库）全 pass
- [ ] smoke test 验证 DeepSeek 仍正常工作（thinking + usage）
- [ ] 架构文档 `providers.md` 更新为 2-client 现实

## 关键设计约束（实现时遵守）

- **不改动 `translate_thinking_openai_compat`**：它已是 thinking 协议的事实策略抽象，OpenAiClient 通过 `config.provider_name` 调用它
- **不改动 `ChatCompletionRequest` 的 thinking 字段**：`enable_thinking`/`thinking_budget`/`glm_thinking`/`reasoning_effort` 保留，OpenAiClient 的 build_request 已正确填充
- **不改动 `Usage::cached_prompt_tokens()` fallback 链**：DeepSeek 的 `prompt_cache_hit_tokens` 仍需处理
- **保留 `LlmConfig::deepseek()`/`dashscope()` 预设**：它们只设 base_url + provider_name，不依赖厂商 client
- **前端零改动**：provider 列表动态获取，后端删 metadata 后 GUI 自动不显示
