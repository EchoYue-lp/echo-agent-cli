---
schema_version: 1
id: asset.eko-conversation-authorities
kind: asset
title: EKO 会话协作与 GUI 展示事实权威
asset_type: state_authority
status: active
risk: high
observed_at: source:76b467a4028b2244e1e73b8c0fb8f57b5c1b0afaec11ddec740493dcf4419397
boundary_refs: [boundary.eko-conversation-collaboration]
code_refs: [echo-agent-app-core/src/lib.rs, echo-agent-app-core/src/api/mod.rs, echo-agent-app-core/src/state/config.rs, echo-agent-app-core/src/agent_router/address.rs, echo-agent-app-core/src/agent_router/recovery.rs, echo-agent-app-core/src/agent_router/router.rs, echo-agent-app-core/src/agent_pool/pool.rs, echo-agent-app-core/src/conversation_archive.rs, echo-agent-app-core/src/managed_conversation.rs, echo-agent-app-core/src/conversation_input.rs, echo-agent-app-core/src/foreground_turn.rs, echo-agent-app-core/src/conversation_deletion.rs, echo-agent-app-core/src/side_conversation.rs, echo-agent-app-core/src/state/app_state.rs, echo-agent-app-core/src/state/workspace.rs, echo-agent-app-core/src/state/tests.rs, src/tauri/commands/conversations.rs, src/tauri/commands/chat.rs, web-frontend/src/stores/chatStore.ts, web-frontend/src/hooks/chatEventHandler.ts, web-frontend/src/types/api.ts]
consumer_refs: [README.md, src/tauri/commands/conversations.rs, src/tauri/commands/chat.rs, web-frontend/src/api/endpoints.ts, web-frontend/src/api/endpoints.sideConversation.test.ts, web-frontend/src/stores/conversationStore.ts, web-frontend/src/components/layout/LeftSidebar.tsx, web-frontend/src/components/chat/ChatPanel.tsx, web-frontend/src/components/chat/MessageBubble.tsx, web-frontend/src/components/chat/SideConversationDialog.tsx, web-frontend/src/generated/AgentGroup.ts, web-frontend/src/generated/SideConversationCreateReceipt.ts, web-frontend/src/generated/SideConversationCreateRequest.ts, web-frontend/src/generated/SideConversationEntry.ts, web-frontend/src/generated/SideConversationGroupMetadata.ts, web-frontend/src/generated/SideConversationStatus.ts, web-frontend/src/generated/TurnVisibility.ts, web-frontend/src/components/chat/ExecutionStatus.tsx, web-frontend/src/components/chat/InlineToolCall.tsx, web-frontend/src/components/chat/NativeMessageTimeline.tsx, web-frontend/src/components/chat/ExecutionStatus.test.tsx, web-frontend/src/components/chat/InlineToolCall.test.tsx, web-frontend/src/components/chat/NativeMessageTimeline.test.tsx, web-frontend/src/hooks/chatEventHandler.hitlReplay.test.ts, web-frontend/src/components/chat/ChatPanel.controls.test.tsx, docs/zh/adr/0057-native-messages-and-progress-policy.md, docs/zh/features.md]
behavior_refs: [behavior.side-conversation-lifecycle]
rule_refs: [rule.conversation-collaboration-authorities]
evidence_refs: [evidence.side-conversation-preflight]
finding_refs: []
candidate_refs: []
---

# EKO 会话协作与 GUI 展示事实权威

## 资产身份

该资产集合表示 EKO 已有 Conversation transcript、AgentRouter group/inbox、durable input、foreground turn、event、deletion 及 conversation archive 可见性权威，不创建新的统一 store。

## 来源与消费者

app-core 拥有服务和文件边界；Tauri 与 React 只消费 typed service 与事件投影。TUI、CLI/JSONL 和 channel 不消费 Side Conversation service。

## 生命周期

Conversation create/restore、input persist/drain/settle、turn cancel、internal delivery、archive/delete 与 workspace shutdown 各由现有 owner 结算。

## 候选关系

GUI branch 和 TUI `/fork` 是不同用户意图的既有普通分叉能力，不发布 Side Conversation relation，也不需要收敛到 Side Conversation service。

## 未知与限制

Side Conversation member metadata、模型覆盖、级联关系和 GUI 投影已有本地门禁与独立复审证据；真实 Tauri 窗口验收和远端主分支交付仍需单独确认。

## GUI 展示状态与执行权威

本轮 `newStateAuthority` 高风险信号按新增显示模型如实声明：`chatStore` 新增原生块缓存、`activity` 操作观察和最终发布呈现标记，仅负责当前 GUI 展示状态；其事实来源仍是原有 canonical 事件、工具执行 repository 和后端 typed turn/TaskRuntime。它不成为执行准入、完成、Goal、PlanTask 或 SubagentRun 的第二权威，不新增 boundary、store、executor 或用户手工协作控制台。

`handleChatEventEnvelope`／`handleAgentEvent` 复用原 `useTauriChat` 当前流准入，保 workspace/conversation/root turn/turn 与事件坐标。正文 Token 不因旧 ThinkStart/ThinkEnd 改型；原生 `assistant_message` 保 `block_id/request_id/sequence/phase/delta/complete/provider/reasoning`。同块按真实请求合并增量，已知阶段冲突、倒退和完成后的迟到不覆盖旧块。`execution_status` 的 Started/Waiting 只驱动模型操作观察；Settled、块 complete 或 provider phase 均不能据此结束 Turn 或 TaskRun。`final_answer` 只从原运行时发布事实接入，拒空并保持首次发布幂等；产品 TurnStatus 与 TaskRun/Goal 继续各走原后端权威。

三处新增／明确的消费者如下：

- `ExecutionStatus.tsx` 从 `chatStore.activity` 读取实际模型/工具操作、起点与最后事件时间。模型请求按实际 request id 分别计时；上游当前流过滤与本地原 root/scope fence 共同防止其它会话通知污染观察。重放使用原 envelope 时间，输入 frontier 通知不更新操作时钟。组件每秒只重绘耗时，不写正文、不推进 lastEventAt、不调用模型或中断工具；原产品结算/历史切换清空观察。ChatPanel 有真实操作行时不叠加旧空白 spinner。
- `NativeMessageTimeline.tsx` 按原 executionSteps 交错显示已交付正文、工具引用和后续正文，Reasoning 单独折叠；缺失 phase 保 unknown，不按措辞或是否带工具猜阶段。`final_answer` phase 候选不公开为最终正文、不开放 Fork，最终正文和操作仍由原运行时发布与原 turn 关闭控制。它是既有消息的呈现消费者；raw provider 历史块没有因此取得发布资格。后端必须提供已批准/已发布投影，不能把 raw Unknown 直接灌进该组件。
- `InlineToolCall.tsx` 复用原 toolExecutionStore／工具详情 reader。仅在 workspace/conversation、真实 chat owner.message_id、call_id、有效名称可唯一对应时消费运行状态；原历史请求/输出另作 display record。缺失／重复／歧义绑定保持 Unknown，不从输出含 Error、已有 UI key 或内容推断成功、失败、耗时或运行。展开详情的迟到 resolve/reject 受原行/owner/call/detail 绑定约束，旧 A 不能覆盖 A→unknown→B 的当前读取。

## 当前源码观察与验证边界

本次 observed_at 使用官方 `verify_semantic.py::source_digest` 实际只读计算的当前源码锚，不使用旧 commit 观察冒新验证。该锚只绑定观察输入；新增结论只覆盖已冻结的 36 个前端及所属中文说明，不把整仓 hash 当 Rust、后端恢复或 native 资格。

真实反例已覆盖：旧 Think 信号误路由正文；首块前失败仍开放 Fork；候选遮蔽后仍留空壳；无关／重放事件刷新操作时间；无 ThinkStart 的连续请求沿用旧起点；真实操作行叠加空白等待行；旧执行说明父组冒已完成；工具详情旧 owner 的迟到覆盖；HITL release 外层四坐标不一致。回归使用正式 handler/store/组件，测试中的合成事件不冒真实模型或远端操作。

本轮完整前端切片第四轮独审 C0/I0/M0，审后原命令实际为 25 文件／260 测试、0 失败、0 跳过；实际 app TypeScript、30 个前端源的 lint 与格式 check 均自然 0。准确 36 路径/逐 SHA、命令、原失败和资格范围在 ignored `parallel-executors-20261027/eko/frontend-complete-handoff-36.json` 及其原日志。HITL 109、Hook/Plugin 19、Cron 36 各是原范围的重叠票据，不相加。原 evidence.side-conversation-preflight 只保其历史场景资格，不替本轮原生消费者验证。

60 秒模型安全点报告策略属于 app-core 后继 Rust；本资产此轮仅绑定其 GUI 时钟/呈现，不据前端通过声称该 Rust 策略或执行结算已验。Guard Transform＋Unknown 多块、取消、Stop 继续的真实冷恢复必须消费 Frame approved 字段与原 ChatEventLog 实际发布/确认事实，当前未验 loader 明确不在本轮切片。完整矩阵、Tauri/WebView、SSH、微信、SDK pin/三语言、精确 main/CI、发布部署均独立未完成。纯前端没有公开模型/工具/SDK API 示例改动，examples 不适用；官网仍说明 native 实现中，未部署。
