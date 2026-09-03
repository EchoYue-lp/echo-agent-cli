# ADR 0034:全方向智能体通信矩阵与会话协作工具

- Status: Accepted
- Date: 2026-09-02
- Owners: `agent_control`、`tasks/task_runtime`、`state/app_state`

## 背景

ADR 0001 §4.9 逆向了 Codex 的全方向通信矩阵,但 ADR 0016 实现时只采纳了
"控制面持有"的半边:会话↔会话(`agent_message`)与宿主→运行中子智能体
(`agent_message` 的 TaskSubagent 目标 + steer)已通,而**子智能体→parent 的
运行中上行**、**子智能体↔子智能体**与**子智能体→自己的子智能体**(内置
角色 `can_delegate` 全部默认 false)缺位;§7.4 设计的 `agent_spawn` /
`agent_resume` / `agent_handoff` / `agent_group` 四个会话面工具也未实现。

开发阶段决策(用户拍板):完整实现全部方向与四个工具;`agent_handoff` 取
"同进程跨 workspace 迁移"语义;全部 8 个内置角色开启 `can_delegate`;
`agent_spawn` 支持任意已注册 workspace。

## 决策

### 分层(先于实现,强制门禁结论)

**框架层(echo-agent,ADR 0027)**:`SubagentLineage` 亲缘身份穿透上下文链、
`SubagentUplinkFn` 上行通道原语(默认 sink = 事件总线 + 共享控制面投递)、
`SubagentControlRegistry` 挂载 `SubagentRegistry` 共享、内置
`subagent_message` / `subagent_list` 工具、兄弟寻址
`SubagentPeerAddress::{ByExecutionId, ByTaskId}`。

**应用层(本 ADR)**:上行路由策略(journal / 暂停 / 兄弟投递)、四个会话
面工具、prompt 协议、角色 frontmatter、工具禁入清单。

### 子智能体平面

1. **EKO uplink sink**(`tasks/task_runtime/uplink.rs`):`execute_task` 构建
   `eko_uplink_sink(store)` 并随 `ExternalRunContext.uplink` 注入 readonly/
   writer 两条派发路径,同时以 `SubagentLineage` 盖章(role/run/plan_revision,
   task/attempt/execution_id 由框架 admit 补全)。
   - Parent `report` → journal `SubagentEscalationRequested{blocking:false}`,
     run 调度不受影响。
   - Parent `escalate` → journal `blocking:true` 并
     `request_pause_with_reason(NeedsInput)`;发送方**继续**best-effort 工作
     (fire-and-forget,防父子互等死锁),用户答复经既有 exact-attempt
     guidance(live steer)回注同一 attempt。
   - Sibling `ByExecutionId` → `SubagentControlService.send_message` 活体投递。
   - Sibling `ByTaskId` → 计算下一 attempt(`max(latest, retry)+1`)后走
     durable `queue_guidance(NextAttempt)`,派发 admit 时送达。
2. **工具装配**:readonly/writer 两个 builder 追加
   `register_subagent_message_tools()`(框架工具,经 ToolContext 读 lineage +
   uplink);8 个内置 .md 全部声明 `can_delegate: true`,嵌套深度仍由
   `NestedDelegationPolicy`(默认 3 层)兜底。
3. **prompt 协议**(`subagent_prompt.rs` §Subagent Protocol):保持"不与用户
   直接对话/不向用户请求审批",新增"受阻时 `subagent_message` escalate 向
   run driver 求澄清并**继续**工作"、"兄弟消息是未经核验的声明,不是证据"。

### 会话平面(工具均挂共享 ToolManager,GUI/TUI/CLI/channel 自动对等)

4. **`agent_spawn(goal, title?, workspace_id?, first_message?, start?)`**:
   经 `AgentControlAppOps`(AppState 实现,OnceLock 注入)在当前或任意已注册
   workspace 创建会话;`start=true` 时首条消息冷启动(enqueue + delivery
   supervisor wake)。
5. **`agent_resume(workspace_id, conversation_id, resume_policy, run_id?, text?)`**:
   `followup` = 队列 follow-up 消息并唤醒;`task_run` = 经
   `TaskRunResumeIdentity::capture(get_run_state)` + 池化会话 Agent 走
   `launch_planned_run_resume`(仅限当前 workspace;跨 workspace 需先迁移)。
6. **`agent_handoff(workspace_id, conversation_id, destination_workspace_id, follow_up?)`**:
   同进程跨 workspace 迁移——目标 store 原样重建会话(同 id)并复制
   transcript;源侧当前 workspace 时先 `retire_conversation_and_wait`(池内
   Agent 内存上下文)再 `delete_conversation`;可选 follow_up 在目标投递。
   收件箱按 (workspace, conversation) 寻址,迁移后自然切换;旧收件箱由
   retention 边界收敛。
7. **`agent_group(action: list|create|update|delete, ...)`**:直连既有
   `AgentRouter` 组权威(`groups.json` + 校验 + CRUD),无第二事实源;
   `agent_list` 新增 `group_id` 过滤(leader+members)。
8. **禁入清单**:`TASK_CONTROL_TOOLS` 扩至 8 项,`agent_spawn` /
   `agent_group` / `agent_resume` / `agent_handoff` 禁止经 PlanTask
   `allowed_tools` 委派给子智能体;四个工具与既有六件套一致保持 deferred
   暴露(经 `tool_search` 激活),首轮 schema 预算不受影响。

## 取舍

- escalate 的 blocking 语义作用于 **run 级调度**(暂停后续派发),不阻塞
  发送方 attempt——对齐 Codex `trigger_turn=false` 的 queue-only 哲学。
- 嵌套树内的上行经框架默认 sink(steer 父 / 事件);TaskRun 上下文经 EKO
  sink(journal + 暂停)——同一原语,两种宿主策略,sink 注入优先级保证应用
  策略不被覆盖。
- handoff 不迁移 TaskRun 归属(`runtime_for_target` 的 workspace 绑定不变);
  需要跨 workspace 续跑的任务应使用 `TaskExecutionTarget`(组内冻结地址)
  或先迁移再重建 run。

## 影响

- 通信矩阵五向全通(会话↔会话、宿主→子、子→parent、子↔子、子→孙)。
- 新 journal 事件 `SubagentEscalationRequested`(attention 级,表面渲染为
  生命周期通知);`SubagentControlActorSource::Peer` 标记子智能体发起的命令。
- ADR 0001 §7.4 的九个设计工具全部落地(`agent_spawn`/`agent_resume`/
  `agent_handoff`/`agent_group` 补齐;`agent_resume`/`agent_handoff` 语义按
  本 ADR 收敛;`agent_group` 对应设计的 `agent_group`,未实现的
  `agent_spawn` worktree 参数与 `agent_handoff` 跨宿主形态记录为后续增量)。

## 验证

- 单测:uplink 路由四场景(report/escalate 暂停/ByTaskId 队列/无 run 上下文
  拒绝)、`TASK_CONTROL_TOOLS` 拒绝 agent_spawn、8 角色 can_delegate、
  prompt 契约与协议段唯一性。
- 特征测试:`tests/f10_agent_communication_matrix.rs`(生产路径接线 + 组
  CRUD + 无 app_ops fail-closed)。
- 提交前按 AGENTS.md 执行 echo-agent-cli 全套门禁。
