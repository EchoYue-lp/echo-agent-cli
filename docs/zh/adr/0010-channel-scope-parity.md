# ADR-0010：Channel 使用 sender-scoped runtime 与精确控制权威

> 状态：Accepted
>
> 日期：2026-08-25
>
> 范围：EKO QQ/飞书 channel 的会话身份、foreground control、TaskRun resume、
> tool projection 与 outbound transport。

## 背景

framework `SessionHandler` 已按 `(channel_id, conversation_id, sender_id)` 创建独立 handler，
但 EKO channel 曾继续用只含 channel/chat 的 `AgentPool` key。不同群成员因此会在应用层重新
汇合到同一 Agent、transcript、TaskRun 和 provider cache。旧 channel 还在收到 `/stop` 或
`/reset` 的 `NoActiveTurn` 后直接删除本地 active pin；若 framework snapshot 仍指向同一 root，
这会在真实 turn 尚未结算时释放产品控制权。

Tool renderer 同时接收即时 `AgentEvent` 与 durable projection。仅维护 `address -> canonical id`
不能发现一个 canonical id 在另一个 address 上重放，尤其是原 address 已进入 recent-terminal
窗口之后；继续渲染会把错误 detail/artifact 引用归给另一个 tool owner。

## 既有依据

- framework [sender-scoped channel session ADR](../../../../echo-agent/docs/adr/0001-channel-session-sender-scope.md)
  已确定通用 session 身份与非法 sender 的 fail-closed 行为。
- EKO [foreground continuation ADR](./0005-foreground-continuation-owner.md) 已确定一个 root owner
  覆盖所有有限 RunTurn，root cancel 与 current-turn steer 分离。
- Tokio `watch`/`CancellationToken` 的既有用法要求 settlement receipt 才能证明 owner 已结束；
  一次 `NoActiveTurn` lookup error 本身不是 settlement receipt。

本 ADR 只组合这些已存在的权威，不新建 session store、TaskRun 状态机或 foreground registry。

## 候选方案

### 方案 A：只依赖 framework 的 per-sender handler

改动最小，但共享 `AgentPool` 仍按 chat key 复用同一 Agent，隔离在应用边界失效。

### 方案 B：每个 channel handler 自建 Agent、TaskRun 与 active map

可以隔离，却复制 `AgentPool`、TaskRuntime 和 `ForegroundTurnControl` 的权威，无法与 GUI/TUI/CLI
保持功能对等。

### 方案 C：应用 adapter 保持薄，并把同一 sender 身份贯穿所有既有权威

framework 继续拥有 SessionHandler 与 opaque incarnation；EKO 对三元身份生成稳定 product
conversation fingerprint，并为每个 incarnation 派生独立 Agent runtime identity。产品 journal、
TaskRun、UI 与 foreground 使用稳定 ID；AgentPool、RuntimeStateStore 与 provider cache 使用
incarnation ID。active pin 只保存已捕获 runtime/root 与 exact agent key，控制动作仍调用
`ForegroundTurnControl`。本项目采用此方案。

## 决策

1. Channel 的 EKO conversation identity 是
   `sha256(JSON[channel_id, conversation_id, sender_id])`。同一 sender 稳定复用；任一坐标不同都
   得到不同的产品 conversation、TaskRun 与 foreground scope。哈希输入使用结构化 JSON，不能用
   可碰撞的分隔符拼接。该稳定 ID 不直接作为模型 runtime key。
2. framework `ChannelSessionInstance` 为每个 handler incarnation 提供 opaque ID，并在直接替换时
   携带 exact previous ID。EKO 将稳定 conversation 与 incarnation 再哈希为
   `agent_conversation_id`；AgentPool key、Agent config conversation、RuntimeStateStore checkpoint 和
   cache user ID 都使用该值。ChatEventLog、TaskRun、router、UI 与 foreground 继续使用稳定 product
   conversation。`AgentInvocationContext.runtime_state_id` 保证 checkpoint save/load 对称，
   `transcript_generation_id` 用 generation + ordinal 幂等追加稳定产品 transcript，不能把旧历史注回
   新模型 context。
3. `ChannelSurfaceIdentity` 保留 typed 三字段，用于 sender-owned active pin。本地 pin 保存 turn
   admission 时捕获的 `ScopedChatRuntime`、EKO conversation id 和稳定 root id；执行期间不重新
   读取当前 workspace focus。
4. framework timeout 创建的新 `AppChannelMessageHandler` 在首次需要新 Agent 前，消费 exact
   previous incarnation 或 sync end callback 留下的 pending obligation。AgentPool two-phase receipt
   先关闭旧 key admission，再等待旧 foreground/lease settlement，最后移除 exact cached Agent。
   waiter 取消会 RAII reopen，pending obligation 不消费。TaskRun cancel/pause、Subagent interrupt、
   HITL 与 `/stop` 等不需要新 Agent 的控制命令必须先执行，避免 retirement 等待其本应释放的旧
   receipt。晚到的 end callback 若看到 replacement 已注册，不得清掉新 incarnation。
5. `/stop` 与 `/reset` 只按 pin 中的 workspace/conversation/root 调用 framework root cancel。
   `Ok(settlement)` 才直接证明 barrier 完成。若返回 `NoActiveTurn`，必须重新读取同 scope snapshot：
   snapshot 仍指向该 root 时保留 pin，stop 要求重试，reset 不得继续；snapshot 消失或已换 root
   时才按 exact generation 清理旧 pin。其它错误一律保留 pin。
6. active pin 由 `ChannelActiveTurnOwner` RAII 与 exact turn-id compare-and-remove 双保险清理；旧
   owner 的延迟 Drop 不能删除 replacement。
7. `/task-resume` 在解析用户选择时 capture 完整 `TaskRunResumeIdentity`（含 journal sequence、
   workspace、conversation），并把同一个 expected 传到 final atomic claim，绝不在 retirement 后
   recapture 同 run-id 的 replacement。fresh retirement 是 framework timeout 已经建立的新 session
   generation 的独立生命周期副作用，不写 TaskRuntime。retirement 后重新 validate 首次 identity；
   从该 validate 到 final claim 不再执行 Agent retirement、attachment staging 或 continuation mutation。
   新增附件更早被拒绝。启用 continuation 的 run 使用 `RunTurnBinding::resume_expected`；已有正式
   plan 且未启用 continuation 的 run 使用 `launch_planned_run_resume`，两者不建立平行 resume authority。
8. EKO `/reset` 在旧 admission 关闭、foreground/lease settlement 与 exact retirement 成功后，
   调用 framework `clear_persisted_runtime_incarnation` 精确回收旧 checkpoint 和任何误写到
   incarnation key 的 transcript，再通过 coordinator 锁驱动 framework instance 原子 rotate，并把
   interaction mode 重置为 Auto、清理 pending HITL。失败或取消不 rotate、不改 mode；authority
   mismatch 进入 quarantine。timeout replacement 的 pending retirement 复用同一 exact GC。reset
   表示新模型上下文，不删除 ChatEventLog、稳定产品 transcript 或 TaskRun。
9. Tool renderer 同时维护 `address -> canonical id` 和 `canonical id -> address`，反向索引覆盖 active
   与 bounded recent-terminal 窗口。任一方向冲突都 fail closed：清除相关临时 entry/detail/artifact
   引用，并 quarantine 所有受影响 address；durable trace 保持事实来源。
10. Channel outbound 继续使用统一 bounded queue、UTF-8-safe chunk、全缓冲 canonical redaction、
    rate policy 与 terminal reservation。transport/renderer 是 channel 差异，chat/TaskRuntime/HITL/
    memory authority 与其它 surface 相同。
11. Channel TaskRuntime 的查询、Goal/requirement mutation、pause/cancel/budget 和最终 resume 校验全部
    通过已有 `TaskRuntimeOperation` 与 store operation supervisor；不得在 Tokio handler 直接
    访问 journal。附件 staging 与 `PreparedUserTurn::build` 在 foreground JoinSet owner 内通过 bounded
    product-data I/O closure 执行，closure 持有 exact workspace receipt。手动压缩 journal safe point
    复用同一 product-data boundary。surface caller drop 不会分离已接受的 blocking work，shutdown
    join 同一 foreground/store owner。
12. 产品 conversation 删除由 EKO tombstone 协调 TaskRun、tool、chat event 与 artifact 清理；最终
    authority commit 调用 framework `delete_persisted_conversation`，一次枚举并删除该稳定 scope 的
    全部 incarnation transcript/checkpoint，最后删除稳定 transcript。`ConversationCommitStarted` 后的
    restart 会携 exact runtime store 重试 helper，不能只删稳定 conversation 而遗留 lineage。

## 影响

- 群聊成员不再共享 Agent context、TaskRun、HITL mode、cache 或 foreground control。
- framework session timeout/reset 会创建新的模型 runtime/checkpoint/cache identity，而稳定产品
  conversation、journal、TaskRun 与可查询历史不变。
- `NoActiveTurn` race 不会提前释放 pin；reset 必须跨过可验证 settlement barrier。
- Tool canonical identity 冲突不会暴露错误 detail 或 artifact 引用。
- EKO product conversation id 为 opaque hash；agent conversation 是其 incarnation 派生值。
- framework public API 增加 `ChannelSessionInstance`、rotation/end incarnation 与 invocation 的
  runtime/transcript generation identity；framework 文档、example 与 echo-website 镜像必须同步。

## 验证

- planned resume 与 continuation resume 走各自唯一生产入口，exact identity 的
  delete/recreate ABA 被拒绝且无前置副作用。
- resume 携带 attachment 被拒绝；普通 attachment 使用 admission 时捕获的 workspace root。
- 真实两轮 continuation 保持 root、推进 active id，并可 steer/cancel。
- stop/reset 的 `NoActiveTurn` + live snapshot race 保留 pin；settlement 后 exact generation 清除。
- 同群不同 sender 生成不同 EKO conversation/cache identity，并由 framework SessionHandler 创建
  不同 handler。
- timeout replacement 的 pending gate 先关闭旧 admission，再等待旧 foreground/execution receipt，
  阻止 ABA acquire；waiter 取消或失败时保留 obligation。inline/prune callback 顺序与晚到 callback
  均保持新 incarnation。
- 同 incarnation checkpoint save/evict/reacquire 可恢复；rotate 后新 Agent 不读取旧 checkpoint。
  identical-tail transcript、重复 safe point、checkpoint/product-store crash cut 与 compaction 后新消息
  都按 generation ordinal 不丢不重；稳定产品历史继续可查。
- reset 成功后 mode 回到 Auto 且只影响当前 sender；retirement/rotation 失败时旧 mode 保持不变。
- reset/timeout 只回收 exact retired incarnation，保留当前 sender 的稳定产品历史、replacement
  incarnation 和其它 sender；产品删除回收该 product scope 的全部 incarnation，restart 可重试。
- compression 与 extraction 使用 channel caller-owned 单一 foreground root；已完成变换优先提交
  journal/typed outcome，`/stop` 在 LLM 执行和 generation retirement 期间都命中同一个 pin。
- TaskRuntime source contract 不存在 async handler 直接 store I/O；附件准备和 compression journal
  不占用 Tokio runtime thread，caller drop 后 exact workspace receipt 持有到 blocking closure 完成。
- 同 address 不同 id、同 id 不同 address（含 terminal 后重放）均进入 typed quarantine。
