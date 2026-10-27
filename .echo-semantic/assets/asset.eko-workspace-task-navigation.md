---
schema_version: 1
id: asset.eko-workspace-task-navigation
kind: asset
title: EKO workspace and ordinary task GUI entrypoints
asset_type: entrypoint
status: active
risk: high
observed_at: source:f49ec6694fa4b4cf0f175f3a746b2a82a610bfd044d86e11e72f88763aa54c7c
boundary_refs: [boundary.eko-conversation-collaboration]
behavior_refs: []
rule_refs: [rule.conversation-collaboration-authorities]
evidence_refs: []
finding_refs: []
candidate_refs: []
code_refs:
- web-frontend/src/App.tsx
- web-frontend/src/components/layout/LeftSidebar.tsx
- web-frontend/src/components/chat/ChatPanel.tsx
- web-frontend/src/components/chat/ChatInput.tsx
- web-frontend/src/components/workspace/NewTaskDialog.tsx
- web-frontend/src/components/workspace/ProjectSourceFields.tsx
- web-frontend/src/lib/workspacePresentation.ts
- web-frontend/src/stores/workspaceStore.ts
consumer_refs:
- docs/en/architecture/overview.md
- docs/zh/architecture/overview.md
- docs/en/architecture/runtime.md
- docs/zh/architecture/runtime.md
- docs/en/getting-started.md
- docs/zh/getting-started.md
- web-frontend/src/App.navigation.test.tsx
- web-frontend/src/components/layout/LeftSidebar.test.tsx
- web-frontend/src/components/chat/ChatInput.thinking.test.tsx
- web-frontend/src/components/chat/ChatInput.submission.test.tsx
- web-frontend/src/components/workspace/WorkspaceDialogs.a11y.test.tsx
- web-frontend/src/components/workspace/ProjectSourceFields.test.tsx
- web-frontend/src/stores/workspaceStore.test.ts
---

# Workspace and ordinary task GUI entrypoints

## 资产身份

工作空间、项目与工程是既有 Workspace。侧栏“新建任务”和“最近任务”投影普通 Conversation，不是另一个 TaskRun graph。App 新任务、命令面板和快捷键复用 conversationStore.startNew，保留 Workspace、原合法 global 会话和后台 Agent；不创建代码目录或第二任务 store。

Workspace registry、原 Workspace/Conversation stores 及后端服务继续拥有创建、切换、持久化和执行。uiStore 的 navigationEpoch 仅表示暂态用户页面意图；原 workspaceStore.exit 仅补已有 isLoading 投影和同 generation finally，不引入第二 transition owner、取消或重放机制。TaskRun/PlanTask/SubagentRun 执行详情和原 Side Conversation 树保留原来源。

## 来源与消费者

ChatInput 的工作空间选择、权限、模型、思考、附件和发送继续调用既有 API。权限模式不控制用户主动目录选择或终端。NewTaskDialog 的历史文件名不改变其实际 Workspace 创建语义；它复用 ProjectSourceFields 和原 createAndSwitch 处理已有本地目录、新建目录或 SSH 设备与远程目录，域用途与位置来源正交。

workspacePresentation 只从原 project_execution_location/project_location 和有效本地项目位置派生显示文本。远程项目显示原 SSH 地址与远端目录，不把本机产品资料 root 冒充远端代码位置。该资产不拥有远程认证、文件系统或进程资源，也不证明它们已完成端到端验收。

## 生命周期

最近任务按有限 Workspace 集合、最多两次元数据请求并发读取原列表，保留 Workspace id/generation 后先完成原切换再加载会话。搜索结果同时在渲染和鼠标/键盘提交前核对原 scope；工作台显式选择使旧页面意图失效。错误保留原诊断，不把旧会话按新焦点投递。

运行中追加输入继续经过原 useTauriChat 与后端输入日志。ChatInput 在附件读取前固定本稿 input id、Workspace generation、Conversation、新任务 epoch 和原始文本/文件快照；单次准入中的重复键盘事件不创建第二输入，未编辑的失败重试保留同一 id 和已编码 payload。首发先分配 canonical conversation id 也不把相同未编辑稿变成第二次提交。

原 Hook 传回真实输入回执后，输入框先观察该事实，再同时检查原 canProject、草稿 revision、附件身份、当前目标和组件存活状态，只有仍属于本次提交的草稿才会清空。原私有 turn/root 身份由 Hook 维护，组件不复制它们。暂态“已收到”只表示原输入日志已接收，不表示已执行、已排空或已完成；普通 Started 与本地斜杠命令采用独立直接接受证明，不伪造持久化回执。ChatPanel 仅将原 Domain 参数与 submission 描述传给既有入口，不新增 Domain 选择器、队列、Store 或状态权威。

## 未知与限制

本机 ignored `.echo-semantic/local/gui-home-navigation/` 保存精确原件：source-epoch10-typed-harness.json、navigation-negative-original-v4 及 exact restore、当前相关六文件67/67和恢复后三文件21/21、whole-app TypeScript、scoped lint/format及frontend build自然0，独立纠正复审 independent-corrective-review-epoch10.json 为 PASS。六个原生产反例以真实行为断言失败，不把先前 test harness 的 DOM/role/类型问题计作反例；全部旧失败保留。

browser-v2 的11张图和原浏览器交互只证明已标识前端/私有 fixture 的布局、菜单与控件，Root 已核浅/深色1400/900窗口；它们不代替真实 Tauri、真实模型或远程服务。后继源码没有继续改变 CSS/布局，原图按自身准确快照保留。历史107测试与本轮67/21存在重叠，不相加宣称独立覆盖。

active 只表示这些 React 入口在当前源码注册并可达。当前提交是以 S1 0607 为父的本地可编前端开发 checkpoint；正式 Plan11 仍 ready，真实 Tauri/HITL/IM/cron/SSH、合流全量门禁、main/CI和发布尚不能由本资产声明完成。旧宽 Rule/Finding/Audit 状态不因本映射而改变。

后继 Plan12 S3 在该 checkpoint 上独立实现与验证。ignored `.echo-semantic/local/steer-draft-feedback/` 的 source-combination-v5-final.json 冻结八份前端源与测试：七个相关文件实际 148/148（Hook 68、ConversationStore 33、输入框提交 19、App 导航 7、输入框思考 3、WorkspaceStore 9、Sidebar 9），全应用 TypeScript、相关八文件 lint/format 和前端构建均自然退出 0。原实现、仅新 Store 配旧 Hook、旧 bootstrap/clear 实现的三组对照分别产生 4、1、8 个真实行为 AssertionError，各组精确还原生产源；三组执行保留相同最终测试字节，不把它们相加称独立覆盖。初始浏览器 fixture 缺失与原可空闭包类型错误原件另存，未当作行为反例，后续原命令已通过。

本批于 2026-10-25T12:27:32Z 在原固定期限内保存并逐文件核对构建归档、结算自有读者、清除独立 node_modules/dist；没有 Cargo 产物，不清其它工作区缓存。S3 独立审查、中央合流与真实 Tauri/模型验收仍需完成。前段 Plan11 的 67/21、browser-v2 及其 native 限制保留各自原快照范围，不能作为新 S3 的工程或视觉证明；当前资产中的验证说明也不把父级 Plan 或 Goal 宣布完成。

## 2026-10-27 首页与明确任务入口调整

用户明确选择首页标题“把想法变成成果”，并要求删除存在多任务歧义的“继续上次对话”卡片，以及顶部“新建支线对话”按钮。`web-frontend/src/components/chat/WelcomeScreen.tsx` 因而只呈现标题、品牌与原输入区，不再查询最后会话；原空建议列表及其唯一回调退出。`web-frontend/src/components/chat/ChatPanel.tsx` 只移除该专用 props、顶部按钮及其唯一本地弹窗状态，不改变发送、附件、Goal/Steer 或会话身份。

继续已有任务仍从原侧栏选择明确 Conversation，普通回复下方的 Fork、已有 Side Conversation 的关系/历史/导航/模型选择与底层 API 保留。此调整只收敛可见入口，不删除共享恢复能力，不新建会话状态权威，也不将待议的 Worktree 分叉视为已实现。

`web-frontend/src/components/chat/WelcomeScreen.test.tsx` 核对标题、未查询/自动恢复最后会话、卡片退出与原图标素材；`web-frontend/src/components/chat/ChatPanel.controls.test.tsx` 核对已选中对话时顶部入口也退出，同时原输入和 Goal 控件可达。`docs/zh/getting-started.md` 同步说明明确任务选择。原观察标识和以上历史测试不改写；当前源码、相关回归、真实原生加载与完整 Goal 资格分别结算，本文不因入口调整声明原生已升级。

## 候选关系

本资产只把当前已注册的 GUI 入口与真实消费者显式关联到原 Conversation collaboration authority，不创建并行能力菜单、持久存储或第二份相同 UI 资产。既有 terminal/channel/plugin/退役人工协作资产的身份和历史保持，各自不承担本首页导航事实。

## 文档、示例与官网

本地双语 overview 的准确导航段落交由中央正式文档 owner 合流。没有新增框架/API 示例合同，现有可执行示例不因布局改动调整；官网镜像由其 owner 在最终集成 EKO 源资格确认后同步，本 checkpoint 不声称官网或正式运行时已同步。
