# 动态 LLM Provider 与模型协议架构

> **状态**：已采纳（2026-08）
> **适用范围**：`echo-agent` / `echo-integration` / `echo-agent-cli`

## 决策

EKO 不再用 OpenAI、Anthropic、DeepSeek、Ollama 等品牌白名单决定连接、认证、
API 协议或端点。用户可以创建任意数量的 Provider；每个 Provider 可以拥有任意
数量的模型。

- Provider 是连接与认证配置：`id`、显示名、API 根地址、可选 API Key、可选
  API Key 环境变量、是否要求密钥，以及添加模型时的默认协议。
- 模型是调用契约：模型名、明确的 wire protocol、输入模态、token/context 参数。
- 每个模型必须明确选择 `chat_completions`、`responses` 或 `anthropic`。
- 每个模型明确声明 `text`、`image`、`audio`、`video` 输入能力；纯文本是默认且始终保留。
- 同一个 Provider 下的不同模型可以使用不同协议和不同输入能力。
- Provider 保存根地址即可。框架按模型协议规范化为 `/chat/completions`、
  `/responses` 或 `/messages`；若输入已经是完整端点，会校验并规范化后缀。

Provider 的 `default_api_protocol` 只用于新建模型表单的初始值，不替代模型自己的
显式协议。Provider 删除前必须先删除其模型，避免无意制造悬空配置。

思考能力是唯一的模型名注册表：它不是连接配置，也不要求用户理解厂商 wire 字段。
框架根据 Provider/endpoint、模型名和已选 API 协议解析一个 `ThinkingProfile`。用户
只在会话中选择当前模型返回的有效等级；`auto` 不发送字段。切换模型时回到 `auto`。
未知模型仍可正常调用，只是不显示手动思考控件。

| 模型范围                      | 用户可选项（另有 `auto`）                          | 说明                                        |
| ----------------------------- | -------------------------------------------------- | ------------------------------------------- |
| GPT-5.6                       | `none/low/medium/high/xhigh/max`                   | `max` 与 `xhigh` 是独立等级                 |
| DeepSeek V4（直连）           | `none/low/high/max`                                | Chat 与 Responses 分别翻译                  |
| GLM 5.2+                      | `none/high/max`                                    | 旧于 5.2 不注册                             |
| Claude 4.6                    | `low/medium/high/xhigh/max`                        | 仅 Anthropic 协议发送 Anthropic 字段        |
| Claude 4.7+                   | 仅 `auto`                                          | 模型自行决定                                |
| Kimi K3 / K2.7 / K2.6         | `low/high/max` / 仅 `auto` / `none/high`           | 分别是等级、模型自行决定、开关              |
| Qwen3                         | `none/high`                                        | UI 只暴露真实的开关语义，不伪造等级         |
| Gemini 3 / 2.5                | `minimal/low/medium/high` / `none/low/medium/high` | OpenAI-compatible 入口                      |
| Ollama GPT-OSS / 已知思考模型 | `low/medium/high` / `none/high`                    | 使用 Ollama `think` 扩展；未知模型仅 `auto` |

## 分层判定

### 通用机制：`echo-agent`

- `LlmApiProtocol`、`ModelInputModality` 及其序列化契约。
- 从 Provider 根地址按协议解析规范端点。
- 三种 wire protocol 的客户端、请求、响应和 SSE 适配。
- 在发送请求前按图片内容和音视频附件类型校验模型声明的输入能力。
- `ThinkingProfile`、中央模型范围解析和各协议 wire 字段翻译。
- Provider 和模型的通用配置结构；不包含 EKO UI 或持久化策略。

### EKO 产品策略：`echo-agent-cli`

- Provider/模型的 CRUD、默认模型选择和文件持久化。
- API Key 与环境变量优先级、连接测试、活动模型的原子发布。
- GUI 表单、TUI slash command、CLI slash command 和错误呈现。
- GUI/TUI/CLI 从同一个 profile 生成可选等级并校验会话选择。
- 本地会话、Task/Subagent 与模型切换的运行时联动。

### 适配边界：`echo-agent-app-core`

`echo-agent-app-core` 将用户 Provider 与模型合成为一个
`ModelRuntimeConfig`，再无损转换为框架 `LlmConfig`。适配器不维护第二份思考或协议
映射，也不重新实现 endpoint 解析。GUI、TUI 和 CLI 的保存、切换及连接测试都经过
同一个 AppState 所有权路径；SQLite 不参与模型配置。

## 数据流

```text
GUI / TUI / CLI
       |
       v
AppState linearized mutation -> EkoConfig file
       |
       v
ModelRuntimeConfig (provider root + model protocol + modalities)
       |
       v
ThinkingProfile + LlmConfig::for_provider -> protocol endpoint -> LlmClient
```

## 实现前重复性审计

全仓库检查发现框架已经有三种协议客户端、`LlmApiProtocol` 和唯一的客户端工厂，
应用也已经有配置文件持久化与活动模型线性化发布路径。因此本次扩展这些权威实现，
没有新增第二套 client、store、mutation owner 或 provider registry。旧的
`ProviderTemplate`、provider 名称推断和 GUI 模板选择路径被直接删除。

## 多模式命令

GUI 在“设置 -> 模型 Provider”中管理 Provider 和模型。TUI 与 CLI 提供相同的
slash command：

```text
/provider list
/provider add <id> <base-url> <chat|responses|anthropic> [api-key-env] [requires-key]
/provider update <id> <base-url> <chat|responses|anthropic> [api-key-env] [requires-key]
/provider delete <id>

/model list
/model add <provider-id> <model> <chat|responses|anthropic> [image] [audio] [video] [default]
/model update <provider-id> <model> <chat|responses|anthropic> [image] [audio] [video] [default]
/model use <model-id|model-name>
/model test <model-id|model-name>
/model delete <model-id>

/think
/think <auto|当前模型返回的等级>
```

CLI/TUI 不接收明文 API Key 参数，避免密钥进入 shell/history；它们使用 Provider
配置中的 `api_key_env`。GUI 可以把密钥写入本地配置，并且更新时默认保留已有密钥。

## 行业参考与取舍

- [OpenAI Codex model provider source](https://github.com/openai/codex/blob/main/codex-rs/model-provider-info/src/lib.rs)
  将连接、认证、headers 和重试放在可配置 provider 中，模型目录另行承载输入模态。
  EKO 采用相同的 provider/model 分离，但因为同时支持三种协议，把协议放到模型上。
- [Continue model config schema](https://github.com/continuedev/continue/blob/main/packages/config-yaml/src/schemas/models.ts)
  允许每个模型选择 provider、model、API base 与能力列表。EKO 将共享连接上提到
  Provider，避免同一网关的密钥和根地址在多个模型中重复。
- Claude Code 官方文档在调研时不可访问，因此没有把无法核验的行为当作设计依据。

连接配置与模型能力是不同维度。EKO 的取舍是“协议归模型、根地址归 Provider”，
这支持一个网关下混合 Chat Completions、Responses 和 Anthropic 模型。输入模态由
用户显式声明；思考 wire 由维护者依据官方文档集中维护，避免让每个用户重复配置
`reasoning_effort`、`thinking.type`、`enable_thinking` 等内部字段。

思考注册表依据：[OpenAI 模型目录](https://developers.openai.com/api/docs/models)、
[DeepSeek 思考模式](https://api-docs.deepseek.com/zh-cn/guides/thinking_mode)、
[智谱参数说明](https://docs.bigmodel.cn/cn/guide/start/concept-param)、
[Kimi 文档](https://platform.kimi.com/docs/overview)、
[Qwen 深度思考](https://www.alibabacloud.com/help/en/model-studio/deep-thinking)、
[Gemini thinking cookbook](https://github.com/google-gemini/cookbook/blob/main/quickstarts/Get_started_thinking.ipynb)、
[Ollama thinking](https://docs.ollama.com/capabilities/thinking)。

## 反模式

- 不得用 provider/model 名称推断 API 协议、端点或环境变量。
- 思考模型规则只能加入中央 `ThinkingProfile` 解析器，不得散落到 GUI/TUI/CLI。
- 不得让应用层复制三种协议客户端或 endpoint resolver。
- 不得让 Provider 默认协议覆盖模型显式协议。
- 不得绕过 AppState 直接写配置并单独刷新 GUI、TUI 或 CLI。
- 不得只给 GUI 增加 Provider/模型能力而遗漏 TUI、CLI。
