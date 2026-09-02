# EKO 配置指南

## 配置位置与优先级

EKO 在 app-core 中拥有 `EkoConfig` 与 `~/.eko` 数据根。runtime bootstrap 只选择
framework `FrameworkConfig` 所需的 provider-neutral 字段、typed `PermissionMode` 和显式
路径；framework 不发现 EKO 配置文件。
`EKO_DATA_DIR` 可以覆盖整个产品数据根，适合测试或隔离实例。

主配置查找顺序：

1. `--config <path>`
2. `EKO_CONFIG`
3. `./eko.yaml`
4. `~/.eko/config.yaml`

GUI、TUI、CLI/JSONL 和 channel 读取同一份 `EkoConfig`。GUI 变更通过统一 AppState mutation
原子保存；包含密钥的配置在 Unix 上写成 `0600`。

EKO channel 通过 `AgentPool` 复用与其它 surface 相同的 provider/model 解析和显式
`LlmClient`，不调用 framework 的 `AgentChannelHandler` 便捷构造器。framework 独立复用方
使用 `AgentChannelHandler::from_config` 时必须显式传入 `LlmConfig`，或使用
`from_config_with_client` 传入已构造的共享 client；不存在默认 LLM 或环境变量回退。

## 推荐配置

GUI 用户优先在“设置 -> 模型 Provider”中管理 Provider 和模型。手写 YAML 时可从
`config/eko.example.yaml` 开始，核心结构如下：

```yaml
model:
  default_model_id: "gateway:main"

model_providers:
  gateway:
    name: "Team Gateway"
    base_url: "https://gateway.example/v1"
    api_key_env: "TEAM_LLM_KEY"
    requires_api_key: true
    default_api_protocol: "responses"

configured_models:
  - id: "gateway:main"
    display_name: "Main Model"
    provider: "gateway"
    model: "main-model"
    api_protocol: "responses"
    input_modalities: ["text", "image"]
    enabled: true
    max_tokens: 8192
    temperature: 0.7
    context_window: 396000

agent:
  name: "echo-assistant"
  max_iterations: 0
  tool_timeout_ms: 120000
  max_tool_output_tokens: 8000
  token_limit: 0
  compress_strategy: "summary"
  compress_window: 20
  subagent_timeout_secs: 600

mcp:
  config_path: null

channels:
  qq:
    enabled: false
    app_id: ""
    client_secret: ""
  feishu:
    enabled: false
    app_id: ""
    app_secret: ""
    mode: "long_poll"
  session:
    timeout_minutes: 60
    reset_keywords: ["重置对话", "新对话"]
    reset_commands: ["/reset", "/clear", "/new"]

webhooks:
  endpoints: []

hooks: {}

logging:
  level: "info"

tui:
  max_display_chars: 20000
```

## Provider 与模型

Provider 保存共享连接与认证；模型保存调用协议、输入能力和模型参数。运行时不根据
品牌名猜协议。

支持的 `api_protocol`：

- `chat_completions`
- `responses`
- `anthropic`

支持的 `input_modalities`：`text`、`image`、`audio`、`video`。`text` 始终保留。

密钥解析顺序是 Provider `auth_token`，然后 Provider `api_key_env` 指向的环境变量。
CLI/TUI 的 Provider 命令不接受明文密钥，避免写入 shell history；GUI 可以把密钥保存
到本地配置。

`default_api_protocol` 只用于添加模型时的默认选择，运行时以模型自己的
`api_protocol` 为准。完整决策见 [Provider 架构](./architecture/providers.md)。
顶层 `model` 只保存 `default_model_id`；connection、credential、protocol、sampling 与
context 字段不会再镜像到这里；该 section 的未知字段会被拒绝。

`agent` section 直接反序列化为 framework `AgentSettings`。EKO 只提供产品默认值，不再
维护平行的 Agent settings DTO。

本地 OpenAI-compatible 服务示例：

```yaml
model_providers:
  local:
    name: "Local"
    base_url: "http://127.0.0.1:11434/v1"
    requires_api_key: false
    default_api_protocol: "chat_completions"

configured_models:
  - id: "local:llama"
    display_name: "Local Llama"
    provider: "local"
    model: "llama3.1"
    api_protocol: "chat_completions"
    input_modalities: ["text"]
    enabled: true
```

## Agent 与压缩

工具、workspace memory 与 human-loop 是 EKO 固定启用的完整 Agent 能力；EKO YAML 不接受
framework 通用的 `enable_tools`、`enable_memory`、`enable_human_in_loop` 开关。

| 字段                     | 含义                                                         |
| ------------------------ | ------------------------------------------------------------ |
| `max_iterations`         | 单 turn 最大 ReAct 迭代；`0` 表示不设产品上限，直到完成/取消 |
| `tool_timeout_ms`        | 单次工具调用超时                                             |
| `max_tool_output_tokens` | 单个工具结果进入模型上下文的预算，完整输出仍可落盘恢复       |
| `token_limit`            | 显式上下文压缩阈值；`0` 使用模型窗口/应用策略                |
| `compress_strategy`      | `summary`、`sliding` 或 `adaptive`                           |
| `compress_window`        | 压缩时保留的近期消息数                                       |
| `subagent_timeout_secs`  | Subagent 派发默认超时；`0` 表示无超时                        |

EKO 的 workspace memory 由文件化 layered memory 管理。不要在应用配置里启用 SQLite
store；framework 的其它复用方是否使用 SQLite 与 EKO 无关。

## MCP

MCP 配置源优先级：

1. `--mcp-config <path>`
2. YAML `mcp.config_path`
3. `MCP_CONFIG_PATH`
4. `~/.eko/mcp.json`

```json
{
  "mcpServers": {
    "example": {
      "command": "npx",
      "args": ["-y", "example-mcp-server"],
      "disabled": false,
      "env": {
        "EXAMPLE_TOKEN": "${EXAMPLE_TOKEN}"
      }
    }
  }
}
```

EKO 对用户配置的 MCP 不做权限级拦截。stdio 命令只做明显输入校验；HTTP endpoint
允许本地/内网明文地址，远程地址要求 HTTPS。Plugin 与用户 MCP 同名时，用户配置优先。
所有配置 mutation 进入 `ExtensionControlService` 并由 `McpConfigRuntime` durable commit +
真实 reconcile；health 按 captured workspace authority scope 保存，不读取 bootstrap Agent 的
全局 map。

## Browser 与 Chrome

Browser runtime 默认启动托管 `@playwright/mcp` sidecar。常用环境变量：

```bash
EKO_BROWSER_ENABLED=true
EKO_BROWSER_HEADLESS=false
EKO_BROWSER_NODE=node
EKO_BROWSER_NPM=npm
EKO_BROWSER_NPX=npx
EKO_BROWSER_MCP_PACKAGE=@playwright/mcp@latest
EKO_BROWSER_PROFILE_DIR=~/.eko/browser/profiles/managed
EKO_BROWSER_OUTPUT_DIR=~/.eko/browser/output
EKO_BROWSER_SESSION_DIR=~/.eko/browser/sessions
EKO_BROWSER_STARTUP_TIMEOUT_SECS=60
EKO_BROWSER_EXTENSION_ENABLED=true
EKO_BROWSER_EXTENSION_TOKEN=replace-with-extension-token
```

`EKO_BROWSER_ALLOWED_DOMAINS` 和 `EKO_BROWSER_BLOCKED_DOMAINS` 接受逗号分隔列表。需要
现有 Chrome 登录态时安装 Playwright Extension，并把扩展 token 配到
`EKO_BROWSER_EXTENSION_TOKEN`。托管 Chromium 与 Chrome extension backend 可同时存在。

## Hooks 与 Webhooks

Hook 合并顺序从低到高：

1. `eko.yaml` 内嵌 `hooks`
2. `~/.eko/hooks.yaml`
3. `<project>/.eko/hooks.yaml`

config watcher 监听 create/modify/remove 和原子保存；Hooks 与 global/workspace
`.lsp.yaml` 通过 `ExtensionControlService` 热发布。修改 model/MCP/runtime topology 仍应通过
对应统一 mutation 或重启，不依赖通用文件 watcher 猜测重建。Hook/LSP reload 使用 admission
时捕获的 workspace project root，process cwd 不参与 workspace identity。

Webhook endpoint 示例：

```yaml
webhooks:
  endpoints:
    - url: "https://example.test/eko"
      events: ["chat_completed", "tool_failed"]
      secret: "replace-me"
```

secret 用于 HMAC-SHA256 签名，不会写入普通事件日志。

## Plugin 与 Skill

- 用户 Plugin：`~/.eko/plugins/`
- Project Plugin：`<project>/.echo-agent/plugins/`
- Local Plugin：`<project>/.echo-agent/plugins.local/`
- 用户 Skill：`~/.eko/skills/`
- Skill 启用状态：`~/.eko/enabled-skills.json`

Plugin 使用根 `plugin.json` 和固定组件位置。Skill 安装、启用和上游同步见
[Skill 同步](./operations/skill-sync.md)。

### Skill desired state 与 settlement

`enabled-skills.json` 是启停策略的唯一 durable desired-state 文件，不另建 repair-debt
store。version 2 schema 包含 monotonic desired generation、canonical
content hash、Skill map 和 bounded recent operation identities。文件必须在同一目录 staging，
完成 file sync 后原子替换并同步父目录；不能使用裸写覆盖作为 durable commit。

durable commit 之后，`ExtensionControlService` 才向 global seed、已加载 workspace 和
AgentPool generation 发布。runtime fanout 失败不回滚已经提交的配置，而是返回带逐 target
状态和 repair debt 的 typed degraded receipt。repair 在 restart、workspace load 和下一次
mutation 前依据 durable desired generation 重放；相同 operation/content 幂等，旧 generation
不能覆盖新 generation。

当前代码已经写入 version 2 `EnabledSkillsConfig`，并通过 ProductData owned flow 与
`atomic_write` 执行上述 durable commit；`SkillSyncReceipt` 区分 committed、settled、
degraded 和 repair debt，并返回 committed file path；结构化 `SkillRepairTargetDebt` 携带
target/component、expected/observed generation、reason 与 retryability。GUI/headless startup
在 Agent delivery recovery 前调用 shared
on-load reconcile，workspace create/switch settlement 也执行相同 repair 并把未收敛 debt
投影为 degraded subsystem。

## 项目指令

EKO 使用标准 `AGENTS.md` / `AGENTS.override.md` 的 root-to-cwd chain，并组合 EKO 自己的
用户/项目/本地记忆投影。应用不会把 `.echo-agent/AGENT.md` 或 `CLAUDE.md` 当成 EKO
项目指令来源。

常用 EKO 文件：

- `~/.eko/user.md`：用户级长期偏好
- `<project>/.eko/learned-rules.md`：已采纳的项目规则
- `<cwd>/.eko/local.md`：本机/目录级说明

`.eko/AGENTS.md` 不是产品指令源，也不会被重命名为 `learned-rules.md`。详见
[ADR 0028](./adr/0028-current-product-schema-authority.md)。

## Channel

QQ 和飞书配置位于 `channels`。环境变量覆盖：

```bash
QQ_APP_ID=...
QQ_CLIENT_SECRET=...
FEISHU_APP_ID=...
FEISHU_APP_SECRET=...
```

飞书支持 `long_poll` 与 `webhook`。Channel 是完整 Agent surface，使用与 GUI/TUI/CLI
相同的 chat driver、TaskRuntime、HITL 和 memory 权威。framework 与 EKO 都按
`channel_id + conversation_id + sender_id` 隔离会话；同一群聊的不同 sender 不共享 Agent
上下文、TaskRun、cache 或 foreground control。timeout/reset 会轮换 Agent runtime/checkpoint/cache
identity，但保留稳定产品 transcript 与 TaskRun；旧 incarnation 在 foreground/lease 与 pool
retirement barrier 后被精确回收。reset 不是产品历史擦除；删除产品 conversation 时才会清理该
scope 的全部 incarnation checkpoint/transcript 和稳定 transcript。

## 常用环境变量

| 变量                         | 用途                               |
| ---------------------------- | ---------------------------------- |
| `EKO_DATA_DIR`               | 覆盖 `~/.eko` 数据根               |
| `EKO_CONFIG`                 | 主配置文件                         |
| `MODEL_NAME`                 | CLI `--model` 默认值               |
| `MCP_CONFIG_PATH`            | MCP 配置文件                       |
| `EKO_UV_PATH`                | analytics runtime 使用的 `uv` 路径 |
| `RUST_LOG`                   | Rust 日志过滤                      |
| `HTTP_PROXY` / `HTTPS_PROXY` | HTTP 代理                          |

Provider API Key 使用各 Provider 的 `api_key_env`，无需维护固定厂商变量白名单。
