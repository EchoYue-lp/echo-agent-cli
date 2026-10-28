# ADR 0047：Agent 自动协作

## 状态

已采纳

## 背景

EKO 已提供模型侧 Agent 控制工具、持久会话投递、关联回复和精确 TaskSubagent 控制。GUI 弹窗又把这些机制呈现为人工选择收件人、投递消息和组管理。用户明确要求 AI 自行组织协作，用户通过 Sidechat 和 Forkchat 参与独立探索。

用户提供的 Codex 交互参考，以及 Claude Code 官方的 [agent-team 消息自动投递](https://code.claude.com/docs/en/agent-teams)，支持由工具驱动协作。EKO 保留自身持久地址和生命周期权威，不要求用户通过 GUI 维护它们。

## 候选方案

1. 保留人工消息/组管理：要求用户组织 Agent。
2. 新增协作动态管理面板，人工操作放次级入口：仍把 Agent 协调作为独立的用户管理流程。
3. 删除协作管理界面，保留自动工具和运行时：符合用户明确的产品边界。

## 决策

采用方案 3。删除 GUI 协作弹窗、组编辑器、聊天入口、无消费者的前端 API/类型及其 Tauri commands；不提供手工地址选择、发送协作消息、组管理或投递收据管理页面。GUI 时间线可以只读呈现 AI 自动协作的真实通信及目标导航。AppState、AgentControlService、AgentRouter 和 TaskRuntime 保留模型工具、消息投递/结算和结果回传。TUI/CLI/channel 控制及通用框架 API 不属于本次 GUI 删除范围。

Side Conversation 创建/导航及 GUI 普通 Fork 继续由用户显式操作。既有对话、任务/Subagent 结果与 HITL 保持原行为。本次 GUI 删除不改写持久数据或消息语义。

## 影响与验证

人工 GUI 有意退役，不声称它与另一个人工 GUI 等价；核心协作能力另由既有运行时/控制测试验证保留。检查 Sidechat/Fork 交互、前端编译/测试、Tauri 注册和适用 Rust 门禁。Framework examples 与 echo-website 不消费已删除 GUI API，无需同步修改。Rust 1.99 原子操作改名另作为编译 lint 修复，保持 ordering、溢出和生命周期语义。

## 2026-10-28：通信事实进入对话时间线

人类以三张GUI原图明确补充：已发送通信显示紧凑一行，点击打开原目标会话；用户与其他Agent的输入均右对齐，Agent来源可见且全文可折叠。本补充替换此前“不新增inbox观察”的界面范围限制，保留不让用户手工组织协作的决定。

应用继续使用AgentRouter的原AgentMessage、耐久投递记录和AgentControlReceipt；不新增inbox、消息store、执行器或协作管理台。原PreparedUserTurn在live/cold实际Message交付之前，将已接纳的typed payload写入现有输入artifact，并以原ResourceLink的应用metadata随canonical Message持久化。GUI只依据该来源呈现，原始来源、message_id、正文和关联资源随当前版本history/reload/Fork保留；模型提示中的wrapper不是GUI来源协议，不解析、迁移或猜测旧wrapper。

单一ChatStore接收只读投递观察。新读取绑定原workspace generation、conversation和观察epoch，旧请求与ABA回到同ID不得覆盖后继。发送行只消费精确工具记录的typed result.data，保留阶段和错误；persisted/queued/accepted不表示已进入上下文、更不表示对方已读。没有typed目标或原目标代际时不猜测导航地址。

右对齐仅为布局。Agent输入继续Runtime authorship，不能批准HITL、更新用户Goal或获得本人编辑/重发权限。工具result、主助手正文及Reasoning保持各自类型。Fork保留canonical来源和资源，用户序号只计真正用户输入，不因Agent输入插入而偏移；不会因为复制历史重新发送或执行消息。

点击目标复用正常workspace switch及conversation load，并核对代际和导航epoch；只导航，不发信、不建会话、不启动Agent。产物侧完整Rust、新版本FileStore cold与实际Tauri原生验收单独取得，JS夹具不冒充该资格。

本实现不消费Framework examples、SDK或官网组件，不更改其公共协议；正式采用由EKO唯一writer在安全切点三方合并共享DTO、store及MessageBubble增量。
