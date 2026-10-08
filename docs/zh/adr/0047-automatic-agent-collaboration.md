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

采用方案 3。删除 GUI 协作弹窗、组编辑器、聊天入口、无消费者的前端 API/类型及其 Tauri commands；不新增协作动态查询、inbox 观察器或管理界面。AppState、AgentControlService、AgentRouter 和 TaskRuntime 保留模型工具、消息投递/结算和结果回传。TUI/CLI/channel 控制及通用框架 API 不属于本次 GUI 删除范围。

Side Conversation 创建/导航及 GUI 普通 Fork 继续由用户显式操作。既有对话、任务/Subagent 结果与 HITL 保持原行为。本次 GUI 删除不改写持久数据或消息语义。

## 影响与验证

人工 GUI 有意退役，不声称它与另一个人工 GUI 等价；核心协作能力另由既有运行时/控制测试验证保留。检查 Sidechat/Fork 交互、前端编译/测试、Tauri 注册和适用 Rust 门禁。Framework examples 与 echo-website 不消费已删除 GUI API，无需同步修改。Rust 1.99 原子操作改名另作为编译 lint 修复，保持 ordering、溢出和生命周期语义。
