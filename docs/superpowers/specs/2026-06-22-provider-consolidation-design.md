# Provider 收敛设计

> **日期**：2026-06-22
> **状态**：已批准
> **目标**：从 9 个 LlmClient 实现收敛到 2 个（OpenAi + Anthropic）

## 背景

当前 `echo-integration/src/providers/` 有 9 个独立 `LlmClient` 实现：

| Client | 文件 | 底层协议 | 状态 |
|---|---|---|---|
| `OpenAiClient` | openai.rs | OpenAI Chat Completions | 保留（基础） |
| `AnthropicClient` | anthropic.rs | Anthropic Messages | 保留（基础） |
| `DeepSeekClient` | deepseek.rs | OpenAI Chat Completions | **删除（冗余）** |
| `QwenClient` | qwen.rs | OpenAI Chat Completions | **删除（冗余）** |
| `GlmClient` | glm.rs | OpenAI Chat Completions | **删除（冗余）** |
| `KimiClient` | kimi.rs | OpenAI Chat Completions | **删除（冗余）** |
| `GeminiClient` | gemini.rs | OpenAI 兼容端点 | **删除（暂不支持）** |
| `AzureOpenAiClient` | azure.rs | OpenAI Chat Completions | **删除（暂不支持）** |
| `OllamaClient` | ollama.rs | OpenAI Chat Completions（本地） | **删除（暂不支持）** |

## 删除依据（已代码核实）

### 4 个国内厂商 client 是纯冗余

`DeepSeekClient`/`QwenClient`/`GlmClient`/`KimiClient` 的全部逻辑可归纳为：

```rust
fn build_request(&self, request: ChatRequest, stream: bool) -> ChatCompletionRequest {
    let t = translate_thinking_openai_compat(&self.model, "<provider_name>", &request.thinking, ...);
    ChatCompletionRequest { reasoning_effort: t.reasoning_effort, enable_thinking: t.enable_thinking, ... }
}
```

而 `OpenAiClient` 已经在做**完全相同的事**（`openai.rs:180-186`）：

```rust
let provider_str = self.config.provider_name.as_deref().unwrap_or("openai");
let t = translate_thinking_openai_compat(&self.config.model, provider_str, &request.thinking, ...);
```

两者唯一区别：厂商 client 硬编码 `provider_name`，OpenAiClient 从 `config.provider_name` 读取。而 `LlmConfig::deepseek()` 等预设已正确设置 `provider_name: Some("deepseek")`，所以 OpenAiClient 能正确处理所有 4 个厂商。HTTP post/stream_post/auth header/usage 解析全部复用 `super::client` 通用函数，与 OpenAiClient 重复。

**结论**：删除这 4 个 client 是纯收益、零功能损失。

### Gemini/Azure/Ollama 暂不支持

这 3 个有真实差异：
- Gemini：`x-goog-api-key` header（非 Bearer）
- Azure：`api-key` header + URL 含 `?api-version=` query param
- Ollama：无 auth

决策：暂不支持这 3 个 provider，直接删除 client 文件 + 从 GUI 选项移除。未来若恢复支持，需评估 auth 差异是否用策略抽象。

## 改动范围

### 框架层（echo-agent）

**删除 7 个 provider 文件**：
- `echo-integration/src/providers/deepseek.rs`
- `echo-integration/src/providers/qwen.rs`
- `echo-integration/src/providers/glm.rs`
- `echo-integration/src/providers/kimi.rs`
- `echo-integration/src/providers/gemini.rs`
- `echo-integration/src/providers/azure.rs`
- `echo-integration/src/providers/ollama.rs`

**修改 `echo-integration/src/providers/config.rs`**：

1. `LlmProvider` 枚举：删 `Ollama`/`Gemini`/`Azure`，只留 `OpenAi`/`Anthropic`
2. `BUILTIN_PROVIDER_METADATA`：删 `gemini`/`ollama` 条目，留 6 个（deepseek/dashscope/openai/anthropic/moonshot/zhipu）
3. `LlmConfig` 预设方法：删 `ollama()`/`gemini()`，保留 `openai()`/`anthropic()`/`deepseek()`/`dashscope()`（预设仍需设 base_url + provider_name）
4. `build_client()`：`LlmProvider::OpenAi` 分支删掉 provider_name match（不再分发到厂商 client），统一走 `OpenAiClient::new`；删 `Ollama`/`Gemini`/`Azure` 三个分支
5. `provider_urls`：删 GEMINI/OLLAMA/AZURE 常量
6. `provider_base_url`/`provider_env_var_names`/`provider_metadata` 等辅助函数：移除 gemini/ollama/azure 分支

**修改 `echo-integration/src/providers/mod.rs`**：删 `pub mod deepseek/qwen/glm/kimi/gemini/azure/ollama` 声明及 re-export。

### 应用层（echo-agent-cli）

**修改 `echo-agent-app-core/src/infra.rs`**：
- `build_llm_config`（1191 行）：删 `"gemini"|"google"` 和 `"ollama"` 分支，兜底分支加 warn 日志提示"provider X 暂不支持，按 OpenAI 兼容处理"
- `provider_required_keys`（934 行）：删 ollama 分支

**前端零改动**：`ProviderPanel.tsx` 从后端 `list_model_templates` 动态获取列表，后端删了 gemini/ollama 条目后，GUI 自动不显示。

### 架构文档

**更新 `docs/architecture/providers.md`**：
- 改为"2 个 client（OpenAi + Anthropic）"反映现实
- 删除"反模式：不为厂商建独立文件"那节
- 记录"暂不支持 Gemini/Azure/Ollama，未来若支持需评估 auth 差异是否用策略抽象"

## 不改动的部分

- `translate_thinking_openai_compat` 函数：保持不动，已是 thinking 协议的事实策略抽象
- `Usage::cached_prompt_tokens()` fallback 链：保持不动，DeepSeek 的 `prompt_cache_hit_tokens` 仍需处理
- `ChatCompletionRequest` 的 `enable_thinking`/`thinking_budget`/`glm_thinking`/`reasoning_effort` 字段：保持不动，OpenAiClient 的 build_request 已正确填充
- `OpenAiCacheAdapter`/`AnthropicCachePlan`：保持不动
- `LlmConfig::deepseek()`/`dashscope()` 等预设：保留（只设 base_url + provider_name，不依赖厂商 client）

## 风险与验证

**低风险**：4 个厂商 client 删除后，OpenAiClient 必须正确处理它们的 thinking 协议。已由现有代码验证——`openai.rs:180-186` 用 `config.provider_name` 调 `translate_thinking_openai_compat`，与厂商 client 硬编码 provider_name 效果完全一致。

**验证步骤**：
1. `cargo build` 全 workspace 通过
2. `cargo test` 既有测试全 pass
3. 冒烟测试：用 DeepSeek（或 Qwen/GLM/Kimi）跑一次对话，确认 thinking 字段正确填充、响应正常

**向后兼容**：已配置 gemini/ollama 的用户（config.yaml 有 `model_providers.gemini`），启动时走兜底 `LlmConfig::new` 分支，会因 auth 差异（如 Gemini 的 `x-goog-api-key`）失效。兜底分支加 warn 日志提示。

## 最终状态

收敛后 provider 层结构：

```
echo-integration/src/providers/
├── openai.rs          ← OpenAI Chat Completions（覆盖 OpenAI/DeepSeek/Qwen/GLM/Kimi 等）
├── openai_cache.rs    ← OpenAI 缓存适配器
├── anthropic.rs       ← Anthropic Messages API
├── anthropic_cache.rs ← Anthropic 缓存适配器
├── client.rs          ← 通用 HTTP post/stream_post
├── thinking_translate.rs ← thinking 协议策略（函数式）
└── config.rs          ← LlmConfig 预设 + ProviderMetadata
```

GUI "模型服务商"页面显示 6 个 provider：DeepSeek / 通义千问 / OpenAI / Anthropic / Moonshot / 智谱 + "自定义"。
