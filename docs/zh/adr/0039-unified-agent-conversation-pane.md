# ADR 0039:统一 Agent 会话分栏与上下文工作台

- Status: Accepted
- Date: 2026-09-04
- Owners: `echo-agent::subagent`、`web-frontend`、`src/tauri`

## 背景

EKO 当前右栏同时承载两套导航:常驻的“任务/分析/研究/浏览器/文件/自动化”工作台 tab，
以及带返回动作的 Subagent 详情栈。选中 Subagent 后只替换正文，原工作台 tab 仍显示选中，
造成两个层级互相冲突，也让 Subagent 看起来像“任务”工作台内的一份详情。

Subagent 详情虽然复用了部分聊天行组件，本质仍是诊断检查器，没有与主 Agent 一致的输入
体验。框架已经产生 thinking、最终 token、工具、usage 和终态事件，但 EKO Tauri 投影会
主动丢弃 thinking/token delta。payload 虽然存在，但公共 Subagent 事件传输没有统一
sequence/timestamp，也不是每个事件都携带完整身份；有界 broadcast 还会在丢事件后报告
lag。executor 当前把内部 `AgentEvent` 已有的 envelope metadata 丢弃后再生成
`SubagentEvent`。因此目标行为需要框架与应用两边共同参与。

业界参考支持把任务状态、执行输出和 Agent 上下文分开:

- [Codex 能力目录](./0002-codex-tool-capability-catalog.md)记录了独立子上下文，以及 typed
  message、follow-up、interrupt、list、wait 操作；
- [Claude Code 能力目录](./0003-claude-code-capability-catalog.md)区分任务关系与后台执行句柄；
- [EKO ADR 0038](./0038-unified-task-subagent-execution.md)将 EKO 固定为一层 Subagent，并
  定义 TaskRuntime 与 direct dispatch 共用的 execution admission；
- [Claude Code Subagents](https://code.claude.com/docs/en/sub-agents)描述独立 Subagent
  上下文、前后台执行与嵌套 Subagent；EKO 只借鉴独立上下文，明确不采用嵌套。

本次决策环境访问 OpenAI Docs 返回 HTTP 403，因此不把未经验证的当前 Codex 桌面布局当作
设计前提。

用户提供的 Codex 桌面截图作为视觉参考而非运行时事实：左侧紧凑任务导航、中间自适应主
Agent 会话、右侧可选上下文分栏、平面底色、细分隔线、克制控件和稳定的底部输入区。

## 候选方案

1. 保留六个 tab，只重新美化 Subagent 详情。
2. 删除 tab 及其背后的全部工作台能力。
3. 删除常驻 tab、保留并重新安置所有能力，同时让选中的 Subagent 使用与主 Agent 相同的
   会话表达语言。

## 决策

采用方案 3。

- 中栏保持主 Agent 会话；点击任意 Subagent 后，在可调整宽度的右分栏显示精确 attempt。
- 主/子 Agent 共用时间线与输入框的展示组件，但通过 typed adapter 保留不同发送语义:
  主 Agent 走普通 turn，运行中的 Subagent 走 exact-attempt message，已结算 Subagent 走
  follow-up。
- follow-up 为同一个逻辑 PlanTask 创建新的 execution attempt；分栏明确显示 attempt 边界，
  不暗示上一个 fresh Agent 实例的私有内存上下文被保留。
- EKO 只允许 `主 Agent -> Subagent`。子 Agent 不注册 `agent_tool`/`task_execute`，runtime
  在深度 1 处创建子运行前直接拒绝继续派生。
- 删除常驻“任务/分析/研究/浏览器/文件/自动化”tab。TaskRun 控制改为上下文运行检查器；
  分析、研究、工作流、结构化提取改为独立工作台；浏览器和文件改为按上下文打开的工具视图。
- 删除“自动化”这一标签，因为当前内容是工作流和结构化提取而非定时调度；两项能力保留各自
  明确名称。
- 右栏只有一个可辨识的上下文目标，禁止 Subagent 选择与工作台选择保留成两套可能冲突的状态。
- 应用采用清爽的三栏结构：可收起任务导航、自适应主 Agent 会话、可调整宽度的上下文分栏；
  右栏关闭时中栏扩展，窄屏使用覆盖层，不能把三栏挤压到一起。
- Agent 页面使用平面中性色、细分隔线、紧凑标题栏、正常阅读字号、icon-first 操作和渐进
  展开；禁止装饰性渐变、超大圆角、重阴影、卡片嵌套和常驻工具 chrome。
- 文件或浏览器上下文可以保留自身必要的局部模式切换，但不得恢复六个全局工作台 tab，也
  不得为每个 Subagent 创建一个 tab。
- framework 把既有 versioned event-envelope 语义扩展到完整 Subagent 生命周期；每个事件
  都带完整 invocation identity、attempt 内单调 sequence、timestamp、稳定 event identity、
  parent correlation 和可检测 gap 语义。
- start/tool/usage/terminal 等权威边界不得静默丢失；thinking/token 临时 delta 可以
  合并，但 gap 必须显式，最终文本由 terminal full output 对账。
- EKO 只向 framework envelope 无损补充 workspace、PlanTask、revision、attempt metadata，
  不在 Tauri 生成替代 sequence，也不新增 Subagent chat store、执行运行时、mailbox 或
  生命周期权威。
- live message 保留 framework tracked-input receipt，EKO 保留 TaskRuntime durable receipt 与
  follow-up guidance 投影；界面只按 exact attempt identity 合并展示，不新建事件状态机。
- 完全相同的终态 error、summary、output、remaining work 只展示一次。

## 取舍

- 真正共享时间线需要 typed view adapter，不能把主 `ChatPanel` 直接嵌入 Subagent 分栏；
  否则会错误继承主会话专属的分支、持久化、slash command 和排队输入行为。成本高于 CSS
  调整，但职责清晰。
- 扩展 framework 公共 event envelope 会增加跨仓库改动，但若在 Tauri 收到事件后才生成
  顺序，就会掩盖上游已发生的丢失并形成第二权威。
- 部分实时 delta 当前并不持久化。重载时只诚实展示权威持久化的子集，不能从终态 summary
  伪造缺失过程；后续若要完整恢复，应扩展现有权威 journal，而不是建前端第二存储。
- 工作台不再常驻一键 tab，但 slash command、命令面板、上下文工具动作和运行状态入口仍
  提供可发现的访问路径，同时不再与 Agent 导航争夺同一层级。
- 参考 Codex 只用于提升空间层级和视觉克制；不复制品牌，也不据此推断 Codex 运行时语义。
  EKO 自身的 TaskRun、attempt、receipt、文件、浏览器和工具合约继续作为权威。

## 影响

- `echo-agent` 负责可复用的 Subagent event-envelope、sequence、gap、terminal delivery 和
  depth 原语，并同步公共文档与 event 示例。
- `web-frontend` 收敛为一个右栏导航权威，建立共享 Agent 会话展示层，不提供嵌套 Subagent UI。
- `src/tauri` 不再丢弃可展示的 Subagent thinking/token 事件，并无损补充 framework 的执行
  身份与顺序。
- 既有 TaskRuntime、Subagent、工具执行和 Agent 控制权威不变。
- 实现时同步更新产品功能文档，并检查官网是否存在旧六 tab 工作区截图或说明。
- `SDK-Docs-Impact`: required；framework 公共 Subagent event 合约与示例会变化。
- `SDK-Skill-Impact`: none；Skill 合约不变。
