# ADR 0042：一级 Side Conversation 复用会话与 Agent 路由权威

## 状态

已采纳

## 背景

EKO 需要让用户从主对话快速创建一个携带上下文快照的支线，在不阻塞主对话的情况下独立运行、继续交互和选择模型。此前 GUI `branch_conversation` 只服务编辑/重新生成，TUI `/fork` 则复制运行时消息并切换到一个普通会话；两条路径没有共同的父子关系、一级限制、模型投影或恢复合同。

当前应用已经具备 file-backed `ConversationStore`、conversation-scoped `AgentPool`、durable `ConversationInputService`、`ForegroundTurnControl`、`ChatEventLog`、`ConversationDeletionService` 和 `AgentRouter` group/inbox。新增 Side Conversation store、消息日志、mailbox 或 executor 会形成第二权威。

Claude Code 官方 Fork 同样把支线作为继承主会话上下文的 Subagent，并允许观察、追加消息和停止；其最终结果自动返回主会话。EKO 采用独立上下文和一级拓扑，但需要长期可寻址的左侧会话树，并明确不自动写回主 transcript。

## 候选方案

1. 新建 SideConversationStore 和专用运行时：关系清晰，但重复 transcript、执行、恢复与取消权威。
2. 把 Side Conversation 建模为 formal TaskRun/PlanTask SubagentRun：可复用任务 UI，但把自由多 turn 会话错误绑定到任务 DAG 生命周期。
3. 组合既有 Conversation、AgentGroup/AgentRouter 和 AgentPool，由一个 app-core service 统一语义，并只通过 GUI/Tauri 暴露（采用）。

## 决策

1. Side Conversation 是相对主 Agent 的一级、可寻址 Subagent 产品角色；它不是 formal PlanTask attempt，不能创建嵌套 Side Conversation。
2. `ConversationStore` 保存 child metadata 与完整 committed transcript 快照。当前 `StoredMessage` 是线性 transcript，没有消息 DAG，因此创建时复制已提交消息并清理源 conversation 专属 UI identity。
3. 每条支线使用一个既有 `AgentGroup`：leader 是主 conversation，唯一 member 是 child，typed metadata 保存幂等 payload hash、可重试的初始 prompt、同步 launch error、模型、snapshot boundary 与 viewed marker。`groups.json` 仍是唯一关系权威。
   Side group 不进入通用 Agent 组列表，通用 update/delete 入口明确拒绝其 group ID，避免绕过支线删除和标题同步规则。Side metadata 更新在既有 groups 文件锁内读取并修改最新记录，model、launch error、title 与 viewed 不会再通过陈旧的整条回写互相覆盖。
4. child ID 和 group ID 由 workspace、parent conversation 与 request ID 确定性生成。同一 request/payload 返回 duplicate receipt；相同 request ID 配不同 payload 明确冲突。
5. Side Conversation 使用原 conversation-scoped AgentPool、input frontier、foreground turn、ChatEventLog 和 deletion service。首轮由 Tauri 以稳定 input/message identity 调用完整 GUI chat driver，因此沿用 GUI sink、HITL、Browser approval、取消和 replay；parent/child identity guard 会持有到首个 GUI foreground lease 注册完成，删除不能利用 create 后的间隙重新产生无 relation 会话。AgentRouter 只负责 Agent 间内部消息。
6. child-local model 是 AgentGroup metadata 的持久权威，AgentPool 只保存可重建投影。变更模型会退休 cached child Agent；模型不存在时明确失败。
7. child 保留普通 Agent 的完整工具能力。Side Conversation 创建只存在于 GUI/Tauri，且 app-core admission 会在任何副作用前拒绝支线 parent，因此一级限制不通过禁用 Task 或普通 Subagent 工具实现。
8. AgentRouter internal message 仍进入模型的 canonical transcript，以便恢复上下文。EKO 在 Agent effect 前把 intent 写入既有 conversation visibility 文件，在 AgentRouter terminal 前结算精确的已提交消息 ID；不为 UI 标签重写 managed `attachments_json`。主 GUI 按 ID 过滤，Side GUI 保留并标注；该投影不依赖 Side group 后续是否删除。运行中 internal delivery 等待独立 cold turn。
9. Tauri 只公开收敛后的 conversation 命令：`list_conversations` 返回可选 relation，`create_conversation` 使用 `primary|side` request，`update_conversation` 使用 `rename|side_model|side_retry|side_viewed` request；get/delete/cancel/send 继续复用既有命令，不保留第二套 `side_conversation_*` CRUD。
10. GUI 左侧呈现 Workspace -> 主对话 -> Side Conversation，显示状态和未读；中心复用完整 Agent timeline/composer。Side Conversation 是依赖侧栏与并行视图才成立的 GUI 布局能力，只通过 Tauri 暴露；TUI、CLI/JSONL 与 channel 不增加专用命令、事件或 wire contract，并保留既有普通 conversation、Subagent 和 `/fork` 行为。

## 失败与恢复

- snapshot 写入失败时回滚新 child；关系发布失败时只删除本次新建 child。
- child 已持久化但首轮启动失败时保留 child、初始 prompt 与 `launch_error`；GUI 使用稳定 ConversationInput/message identity 显式重试，active 或 terminal 首轮直接返回既有状态，成功后清除错误且不会重复执行。
- 创建 relation 与父级联删除共享 parent/child identity guard；删除 active child 先取消并等待 terminal，再进入 aggregate delete，父 receipt 聚合所有 child 的 cleanup debt。
- create/retry 回执携带稳定的初始 prompt identity，使慢首轮也立即出现在选中时间线；已完成 transcript 按 snapshot boundary 和初始 prompt 重建同一 identity，快速完成或响应丢失重试后不会重复投影。状态轮询使用独立于 transcript load 的 generation，当前可见 child 会推进持久 viewed marker，完成后不会产生伪未读。
- 缺失 child 的 group 作为 degraded relation 显示，不重绑到同名 conversation。
- 删除 child 先完成现有 aggregate deletion，再删除 relation；删除 parent 先逐个删除 child，再删除 parent。GUI 明确显示级联范围。
- internal message 继续使用 AgentRouter typed receipt，不把 persisted 误报为 completed。
- effect 后、delivery terminal 前崩溃或 visibility 写失败时，durable intent 与非终态 AgentRouter 前沿继续保留。GUI 读取前可用 Router 记录和该回合时间边界内的精确 transcript 行修复；证据缺失或歧义时明确失败。visibility 成功结算后才写 Router terminal，terminal 保留窗口淘汰后 ID 仍可独立读取。替换 transcript 推进 epoch 后，旧来源 ID/intent 随之退役。

## 影响

- Side 专属拓扑与 GUI 语义仍属于 EKO；独立的通用框架 managed import generation/checkpoint CAS 合同由框架 ADR 0080 记录。
- EKO 新增 app-core Side Conversation service、AgentGroup typed metadata、AgentPool child-local projection、Tauri conversation request、GUI adapter 与生成 TypeScript contract。
- 多模式对等继续约束核心 Agent 能力；纯布局/窗口编排可按 surface 独有。该例外不允许用来删减 TUI、CLI/channel 的任务、Subagent、工具、HITL、记忆或附件能力。
- framework examples 无需更新，因为没有公共 framework 合同变化。
- `echo-website` 不发布 EKO 当前应用 UI 或本次应用层 API，因此无需同步。
- 正式行为见 `docs/supreme/specs/side-conversation/design.md`；该设计也记录当前 main 不存在 `ConversationDomainData` 类型或消息 DAG 的事实纠偏。
