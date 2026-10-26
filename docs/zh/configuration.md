# EKO 配置指南

## 最大子智能体层数

在 GUI 配置页或 EKO 独有的顶层 YAML 字段中设置。只接受整数 1、2、3，默认 1；主智能体为第 0 层。仅对新运行/turn 生效，已运行任务及同 Run 新任务、重试和恢复保持原绑定。非法配置文件明确失败，不静默加载默认配置。

```yaml
max_subagent_depth: 1
```

这不是并发数，不改变框架默认，也不要求轻量 agent_tool 创建 TaskRun。历史缺少可信绑定的运行不能从当前设置获得新委派许可。见 [ADR 0052](adr/0052-gui-subagent-depth-policy.md)。

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
  compress_window: 0
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
  weixin:
    enabled: false
    account: null
    notifications: {}
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

`/compress [重点]` 与 `/compact [重点]` 都使用当前配置的策略；TUI 支持同样的重点文本，
GUI 压缩面板提供可选输入框。旧 `keep_messages` 参数已移除，命令不覆盖已安装的预算。
最新请求超过软预算时原文可以扩展保留，超过硬输入预算则报错。GUI 的 75% 提示只是建议；
实际压缩在模型调用前扣除工具/格式定义与 protected 内容后判断。
Goal、recovery capsule 和最近用户 steer 由 TaskRuntime projection 保留，完整历史仍由
ConversationStore 保存。详见 [ADR 0048](./adr/0048-context-compression-product-policy.md)。

工具、workspace memory 与 human-loop 是 EKO 固定启用的完整 Agent 能力；EKO YAML 不接受
framework 通用的 `enable_tools`、`enable_memory`、`enable_human_in_loop` 开关。

| 字段                     | 含义                                                         |
| ------------------------ | ------------------------------------------------------------ |
| `max_iterations`         | 单 turn 最大 ReAct 迭代；`0` 表示不设产品上限，直到完成/取消 |
| `tool_timeout_ms`        | 单次工具调用超时                                             |
| `max_tool_output_tokens` | 单个工具结果进入模型上下文的预算，完整输出仍可落盘恢复       |
| `token_limit`            | 显式上下文压缩阈值；`0` 使用模型窗口/应用策略                |
| `compress_strategy`      | `summary`、`hybrid`、`sliding` 或 `adaptive`                  |
| `compress_window`        | 默认 `0`：近期原文按窗口 25%、至多 20K token 选择完整 turn；正数显式使用旧消息条数上限 |
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

config watcher 监听 create/modify/remove 和原子保存；Hooks 通过 `ExtensionControlService` 热发布。修改 model/MCP/runtime topology 仍应通过
对应统一 mutation 或重启，不依赖通用文件 watcher 猜测重建。Hook reload 使用 admission
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
- 项目 Skill：`<project>/.eko/skills/`
- Skill 启用状态：`~/.eko/enabled-skills.json`

Plugin 使用根 `plugin.json` 和固定组件位置。Skill 安装、启用和上游同步见
[Skill 同步](./operations/skill-sync.md)。

### Skill 启用状态与 settlement

`enabled-skills.json` 是外部 Skill 启停策略的唯一持久文件。version 3 schema 只包含
flat Skill map，每项为 `{enabled}`，通过同目录 staging 与 `atomic_write` 原子替换。
旧文件中的 `category`、`baseline`、generation、operation identity、content identity 与
repair debt 字段会被忽略，但已有外部 Skill 的 `enabled` 选择会保留。损坏或不可读配置
回退空集合并记录 warn；EKO 不再生成默认启用集。项目 Skill 由当前项目的运行时直接发现，
不写入用户启用文件，也不受用户 Skill 的卸载操作影响。

`ExtensionControlService` 在同一 mutation 锁内写文件，再向 global seed、已加载 workspace
和 AgentPool 发布。`SkillSyncReceipt` 只返回本次操作的 `Settled` 或 `Degraded` 即时结果及
逐 target 错误。文件已写但运行时同步失败时不回滚配置；下一次 Skill 操作、应用启动或
workspace load 会重新读取 flat policy 并收敛，不保留精确重放状态。EKO 不扫描源码树
`skills/`，也不向 Agent 注入方法论 baseline；仓库 `.agents/skills/` 仅用于开发协作。

## 项目指令

EKO 使用当前项目标准 `AGENTS.md` / `AGENTS.override.md` 的 root-to-cwd chain，并组合 EKO 自己的
用户/项目/本地记忆投影。应用不会把 `.echo-agent/AGENT.md` 或 `CLAUDE.md` 当成 EKO
项目指令来源。

常用 EKO 文件：

- `~/.eko/user.md`：用户级长期偏好
- `<project>/.eko/learned-rules.md`：无独立 workspace 时的已采纳项目规则；链接 workspace
  使用 `<workspace>/.eko/learned-rules.md`
- `<cwd>/.eko/local.md`：本机/目录级说明

项目根及其子目录中的 `AGENTS.md` 会进入同一个 instruction projection；链接 workspace 的
项目根由 `Workspace.project_root` 指定。`.eko/AGENTS.md`
不是产品指令源，也不会被重命名为 `learned-rules.md`。详见
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

### 个人微信（源码候选，待编译与原生验收）

个人微信使用 `channels.weixin`，通过现有渠道页的扫码交互连接，不填写微信密码、AppID 或 AppSecret。
二维码由本机编码，手机需要数字校验时只在专用交互提交。刷新/取消绑定原 challenge，迟到 confirmed 不得覆盖新配置。
`account` 中的 token、账号、扫码用户和 credential generation 由后端在原 ConfigState owned save/CAS 成功后发布；普通配置、状态与 Debug 不返回 token、context、二维码内容或数字码。
配置文件保留既有本机受限权限，不把凭据写入模型输入或日志。手写默认空态可使用：

```yaml
channels:
  weixin:
    enabled: false
    account: null
    notifications: {}
```

当前源码区分“已配置/已授权”、authenticated poll 的 Ready 和一次发送的 Accepted；保存成功或启动成功不代表在线，Accepted 不代表手机已收件。
渠道页仅在原传输状态为 `ready` 且证据为 `weixin_authenticated_poll` 时显示“微信鉴权轮询已确认”；只有授权、正在连接或缺少证据时不显示该确认。微信连接区同时保留原传输错误、已结算的清理失败记录和观察时间，清理历史不会覆盖当前连接事实。
`-14` 要求重新认证；清除只移除本机该登录/协议状态，不宣称服务端撤销，也不删除原会话历史或未结算通知。
首版一个 active 账号，只接受扫码本人对应的 direct 单聊文字进入原 Agent/TaskRun/HITL；换账号不借旧 context、会话或 HITL 身份。
群聊、媒体及混合媒体不执行残余文字。不安装 OpenClaw Gateway，也不接管桌面微信。

定时任务页可为准确的 task definition 启用微信结果通知，目标为已连接账号的扫码本人；需先从手机发过单聊以取得原 context reference。
`notifications` 属于 EKO 配置，不向通用 CronTask 添加产品字段。原 occurrence 准入在同一 TaskRuntime journal 提交中冻结 account、credential generation、peer、context reference、workspace、channel conversation 和 definition。
之后改配置、切项目或重新授权不重选已准入目标。真实完成结果与产物引用沿原 DeliveryLedger/TaskRuntime journal 投递；通知失败或 Unknown 不改任务结果、不重跑 prompt，潜在已发送的 Unknown 不自动重发。
窗口、额度、无 context 或重启后主动发送是否被服务接受均须实际验证，不能承诺必达。

控制完成依据原效果 owner 的类型化结果，不依据渲染文字或流 EOF。原 owner 确认未准入时，同一 provider ID/payload 持久 Deferred，暂不 ACK 以便重试；部分或不明效果先持久记录 Unknown，再发布恢复回执，不能自动重放。原生渠道命令显式使用 snake_case 参数，与既有 endpoints、Tauri bridge 一致。

原已配置渠道启动完成后，恢复入口扫描全局及已登记 workspace 的原 Completed run journal，使用冻结 occurrence/target 身份补回从未开始的通知义务，不重跑 prompt、不改变 TaskRun Completed。既有 Accepted 不重发，已开始且效果不明仍保留 Unknown；缺失身份或持久化证明继续报告诊断。

原 input pump 的微信回复 route 在等待 claim 时只保留 receiver、精确消息 context 和 fenced 消息模板数据，不保留 response task、SessionStreamReceipt 或 delivery permit。原单一 pump claim 后才并行驱动同一 foreground 与回复 consumer，通过原 bounded sender 投递；不新建微信 inbox。context 的显式 None 也按原消息身份固定，不借之后的 peer token。

**压力验证待执行：**上述原 pump 数据 route 针对旧路径的“32 个普通响应槽等输出→严格 cursor frontier 卡住→后续 `/reply` 不可达”结构性反例。显式 Steer 保留原 tracked live 注入；原持久接收及 supervised observer 已拥有该输入后立即返回“已持久接收，等待原任务处理”，不等待 drain 或追加同轮输出 waiter。后续实际 drain/recovery 仍由原 owner 记录，不把接收 ACK 当已应用。超过 32 条及后续 `/reply` 的压力回归、编译和原生检查仍未运行，不能把源码接线当作已验证闭合或完整微信/HITL Outcome 已通过。
本候选尚无编译或原生验收；本地 fixtures、前端测试、真实 GUI 扫码/手机收发与 main CI 分别结算。真实登录与发消息只由用户在最终 GUI 操作。
协议参考腾讯第一方 [openclaw-weixin 874637a0，2.4.10，MIT](https://github.com/Tencent/openclaw-weixin/tree/874637a0dbc24050f72d1385add4fc6170a2d726)。详见 [ADR 0056](adr/0056-personal-weixin-channel.md)。

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

旧 `.lsp.yaml` 不再被解析或热重载；发现时只报告 LSP 已退役，保留文件原件。

## IM 人工请求的回复身份

EKO 渠道中的每个活动审批、输入或选择提示都会给出 response target。使用 `/reply <response-target> <answer>` 回复，使用 `/reply <response-target> /cancel` 拒绝。审批继续支持 `y`、`a`、`n [reason]`、`m [feedback]`；选择沿原选项数字或名称；自由输入保留 answer 后缀的中文、emoji 与换行。有 pending 时裸文本不回答请求。缺失、旧、非 front 或其它会话的 target 明确拒绝且不进入普通聊天；无 pending 的普通文本仍可聊天。

submitted 不等于原 provider/dispatcher 已选定的 effective answer。GUI 回复携带原投递 scope/target，不因切页重绑，缺 target 的历史事件不重新激活。合同与待完成的 Rust/native、实际双端组合及 IM/cron 验收边界见 [ADR0053](adr/0053-im-cron-gui-control.md)。身份源码 checkpoint 不表示完整渠道管理页或原生平台投递已验证。

## 领域方法声明

已启用插件可在包根增加一份 `domain.yaml`，声明 `schema_version: 1`、非空 `method`、
可选 `default_subagent_roles`，以及明确的 `allowed_tools`、`required_artifacts`、
`execution_checks`、`acceptance_criteria`、`review_checklist`、`inputs` 列表。未知字段
或非法声明沿原发布错误路径拒绝候选；无声明仍是普通插件。名称和可选版本只来自原
manifest，不在此重复。

原聊天/任务请求接受可选 `domain_plugin`（GUI 为 `domainPlugin`），省略保持 General。
原输入队列把选择与 payload identity 一同持久化，steer 不能换掉活动 Run 的领域。
新非 General `domain_profile` 请求须明确选择插件；历史枚举可读，但不再授权新专业执行。

inputs 为规范化包内相对文件。选定包/依赖内容、monitors、output style 及实际全局
style provider 进入绑定，纯 theme 颜色不进入。来源缺失或变化阻止新执行，不覆盖原规则；
已知敏感值不得复制进绑定/journal。手动终端/MCP 操作不受该 Agent 政策控制。
见 [示例](../../examples/domain-plugin/README.md)。当前源码未编译，adhoc 后台委派仍未闭合。

微信保存但从未开始的输入，在 authenticated Ready 后仍回到原工作空间和消息 context；已尝试或无法确认的工作保留待恢复。恢复沿用原已提交前缀保证，未提交 incomplete tail 与损坏或已 claim 的工作分开处理。当前源码候选尚无构建或原生验收。
