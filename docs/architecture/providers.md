# LLM Provider 与协议架构

> **状态**：已采纳（2026-08）
> **适用范围**：`echo-agent` / `echo-integration` / `echo-agent-cli`
> **决策性质**：架构约束，偏离需显式评审

## 决策

供应商身份和 wire protocol 是两个不同维度：

- `provider` 表示供应商身份，用于默认端点、认证来源、模型能力和 thinking 策略。
- `LlmApiProtocol` 表示端点实际使用的协议，目前为 `responses`、`chat_completions`、`anthropic`。
- `ProviderMetadata.default_api_protocol` 是内置供应商默认协议的唯一权威。
- `LlmConfig::build_client()` 只按 `api_protocol` 选择协议适配器，不再按供应商名称硬编码客户端。

内置默认值：

| Provider | 默认协议 | 默认端点 |
|---|---|---|
| OpenAI | Responses | `https://api.openai.com/v1/responses` |
| Anthropic | Anthropic Messages | `https://api.anthropic.com/v1/messages` |
| DeepSeek / Qwen / Kimi / GLM 等 | Chat Completions | 各自兼容端点 |
| Custom | 由 URL 推断，也可显式指定 | 用户配置 |

## 分层判定

### 通用机制：框架层

以下能力对任何使用 `echo-agent` 的应用都成立，放在 `echo-agent`：

- 三种 wire protocol 的请求、响应和 SSE 适配器。
- 文本、图片、文件、结构化输出、function call、并行 tool call、reasoning、usage 与取消。
- Responses 加密 reasoning item 的保存和后续回放。
- 完整 raw Responses create/stream 接口，保留未知字段和语义事件。
- `ProviderMetadata` 中的供应商身份、默认端点、环境变量和默认协议。

### EKO 产品策略：应用层

以下是本地个人助理的产品选择，留在 `echo-agent-cli`：

- GUI/TUI/CLI 的模型选择与配置持久化。
- 把框架 provider metadata 和用户显式 override 无损投影到 GUI/TUI/CLI。
- EKO 以本地会话历史为权威，不依赖远端 conversation 状态。
- EKO 的工具和 Task/Subagent 运行时仍在本地执行。

### 适配边界

`echo-agent-app-core` 只把模型配置转换为 `LlmConfig`，不维护 provider-to-protocol 映射。解析顺序固定为：用户显式 `api_protocol`、框架可识别的完整 endpoint 协议推断、框架 `ProviderMetadata.default_api_protocol`、unknown provider 的 Chat Completions fallback。协议语义、默认值和 endpoint 推断全部由框架提供。Auto 模式允许保留无法单独识别协议的 provider 根地址；显式协议则要求完整 endpoint，且后缀必须匹配。应用在启动/保存/测试连接前执行这一校验；本地、兼容 override 或 unknown provider 不强制 API Key，但仍注入同一份运行时配置。

## Responses 支持范围

### Agent 高层路径

`ResponsesClient` 把 provider-neutral `ChatRequest` 映射为 Responses：

- 完整本地消息历史映射为 `input` items，system/developer/user/assistant/tool role 不丢失。
- 图片映射为 `input_image`，文件映射为 `input_file`。
- function tools 使用 Responses 的扁平定义；tool output 使用 `function_call_output`。
- `max_tokens`、temperature、tool choice、JSON schema、reasoning effort 和 prompt cache key 使用 Responses 对应字段。
- 请求固定 `store:false`，并请求 `reasoning.encrypted_content`；加密 reasoning item 会存入消息并在下一轮回放。
- 非流式响应保留原始 `Response` 对象；流式按语义事件解析，不依赖 Chat Completions 的 `[DONE]`。
- input/output/total、cached/cache-write 和 reasoning token usage 均归一到框架 `Usage`。

这条路径刻意不使用 `previous_response_id` 或远端 conversation，因此不会改变 EKO 本地历史、压缩和恢复模型。

### 完整低层路径

`ResponsesClient::create_raw()` 和 `create_raw_stream()` 接受完整 JSON 请求并返回完整 JSON 响应/语义事件，不裁剪 schema。调用方可使用 Responses 的 hosted tools、background、conversation、metadata、service tier、moderation、context management 及后续新增字段，而无需把这些协议专属概念塞进 `ChatRequest`。

## 为什么 Responses 不破坏缓存

Responses 并不要求使用服务端状态。`input` 可以携带完整历史，`store:false` 可关闭存储，`reasoning.encrypted_content` 支持无状态 reasoning 回放，`prompt_cache_key` 继续提供缓存分区。因而 EKO 可以同时保持本地历史权威、稳定前缀和 provider prompt cache。

## 参考实现与取舍

- [OpenAI Responses create reference](https://developers.openai.com/api/reference/resources/responses/methods/create)：确认完整请求/响应 schema、`store`、`input`、tools、reasoning、usage 和流事件模型。
- [OpenAI Node SDK Responses resource](https://github.com/openai/openai-node/tree/master/src/resources/responses)：交叉核对官方 SDK 的请求类型、输出 item 和 streaming event union。
- [DeepSeek Responses API 指南](https://api-docs.deepseek.com/zh-cn/guides/responses_api)：仅用于独立验证兼容协议的 input item、function call、usage 和语义 SSE 事件；本项目没有 DeepSeek Responses 专属分支。

跨实现的共同模式是：Responses 是独立 wire protocol，流使用具名语义事件，工具调用由 output item 与 call ID 串联。项目据此保留一个 provider-neutral `LlmClient` 高层接口，再提供 raw 接口承载协议完整能力。

## 文件结构

```text
echo-integration/src/providers/
├── responses.rs          # OpenAI Responses 高层映射 + 完整 raw API
├── openai.rs             # OpenAI-compatible Chat Completions
├── anthropic.rs          # Anthropic Messages
├── client.rs             # 共享 HTTP 与 UTF-8-safe SSE transport
├── thinking_translate.rs # thinking 配置翻译
└── config.rs             # provider 默认值、协议选择与 client factory
```

## 新供应商接入

1. 确认供应商实际 wire protocol，而不是根据品牌猜测。
2. 兼容现有协议时只在框架 `ProviderMetadata` 增加 provider/default，不在 EKO 复制映射或新增 client。
3. 自定义服务可显式设置 `api_protocol`；省略时按完整 endpoint URL 推断。
4. 只有协议结构无法被三种现有适配器表达，且不是单一厂商扩展字段时，才评估新增协议。

## 反模式

- 不得按 provider 名称复制 Responses/Chat/Anthropic 客户端。
- 不得在 EKO 增加第二套 provider 默认协议 helper/switch。
- 不得把 `previous_response_id` 作为 EKO 本地多轮对话的必需状态。
- 不得把 hosted tool、background 等协议专属字段逐个塞进 provider-neutral `ChatRequest`；使用 raw Responses API。
- 不得在应用 adapter 重做 SSE、tool-call frontier 或 reasoning 回放。
