# ADR 0009: TaskRuntime Async I/O And Typed IPC Boundary

状态：已采纳

## 背景

TaskRuntime 以本地文件 journal 为权威。`TaskRuntimeStore` 的同步 API 会打开或恢复 journal、
执行 `fsync`、刷新 checkpoint 和读取投影，因此不能直接运行在 Tokio async executor thread 上。此前 DAG
executor 已通过进程共享、固定上限的 `TaskRuntimeBlockingAdapter` 使用
`tokio::task::spawn_blocking`，但 chat preparation、background submit、revision adapter 和
Tauri commands 仍有同步调用。

同一时期，GUI mutation command 仍返回 `serde_json::Value`，interaction mode 仍通过 `u8`
传输。Rust 与 TypeScript 无法在编译期发现 receipt 字段漂移；continuation resume 已经实际
返回 `turn_id`，而前端手写类型没有声明该字段。

## 候选方案

1. 保留同步调用，依赖 Tokio 多线程隐藏延迟。实现最少，但冷恢复和大 journal 会占住异步
   executor thread，并使多个 GUI polling command 相互放大。
2. 新建一套 async TaskRuntime store。调用形状直观，但会产生第二个 I/O 调度和 authority
   边界，增加 mutation 语义分叉风险。
3. 复用现有 bounded `TaskRuntimeBlockingAdapter`，在 async 边界组合完整同步 transaction；
   IPC receipt 由 Rust DTO 和 ts-rs 生成。本项目采用此方案。

## 决策

- 所有 async production boundary 只通过 `TaskRuntimeBlockingAdapter` 执行 journal/projection
  文件 I/O。进程共享 semaphore 提供固定并发上限；每个 `TaskRuntimeStore` 的 operation
  supervisor 才是已接受 operation 的生命周期权威。adapter 不拥有业务语义。
- 复合 mutation 在一个 blocking closure 内完成，避免同一操作在 Tokio async thread 与 blocking
  pool 之间来回切换。
- `spawn_blocking` 一旦启动就不能被 async future 中止。blocking handle 由 store supervisor
  await，调用方只等待 oneshot receipt；caller drop 不会 detach 所有权。多阶段 Subagent
  command 和 turn projector 也由同一 supervisor 托管，允许已经接受的 command 在 shutdown
  admission 关闭后继续进入 nested terminal settlement。
- Application lifecycle 的 phase one 同时关闭 global/workspace store 的新 operation admission。
  admission seal 与 reservation 注册在同一锁内线性化；seal 后不存在可在 join 返回后复活 active
  计数的 `accepted` 后门。CommandCell 在 framework `prepare_launch` 前预留 operation，RunDriver
  在发布 driver owner 前预留 operation。phase one 前已接受的 RunDriver、CommandCell observer、
  Subagent command 和 turn projector 持有 nested settlement capability，仍可写 terminal。
- phase one 同时启动 framework command manager 的 shutdown owner，立即关闭 command admission 并
  取消长命令；phase two 才以 30 秒上限 join operation。CommandCell terminal 与 Awaiter Ready
  采用有限 repair budget；耗尽后留下 projection debt，由 command-cell/application lifecycle
  typed receipt 报错。observer join 也有总 deadline，不会在 operation timeout 后再次无限等待。
  Workspace eviction 把整段 observer 计入 active operation，因此不能在 command 结束和 terminal
  落账前移除 authority。
- Subagent Queued/Requested 后的 framework panic 会转换为 typed rejection；terminal append 由
  store owner 重试直到提交或 lifecycle timeout，不能把半提交 command 当作成功返回。
- revision store 与 EKO task policy 继续保持薄 adapter：framework 仍拥有 patch、DAG、CAS
  和 revision 语义；blocking boundary 不引入第二套 validator 或 executor。
- Tauri mutation 返回 `TaskRunControlReceipt`、`TaskRunResumeReceipt` 和 `TaskRetryReceipt`。
  continuation resume 的 `turn_id` 是显式可选字段。Surface capability 来自注册工具与
  workspace facts，不再暴露 interaction-mode wire contract。
- 排队 resume 的 identity 同时冻结 continuation route。GUI/TUI/CLI 只校验 workspace 与
  conversation scope，不读取或预判当前 journal sequence；ABA、status 和 sequence 的唯一判定在
  store 原子 resume transaction。capture 后仅追加 `kind=execution_path` diagnostic Note 时允许
  resume，其它事实 suffix 仍拒绝。
- ChatEventLog 的 Awaiter Ready、GUI cancel 与 HITL orphan 投影通过 bounded product-data I/O
  adapter 执行，不在 Tokio executor thread 直接做文件 append。GUI/TUI/CLI/channel 共用的 manual
  compression safe point 也走同一边界。每个 blocking closure 捕获 exact workspace I/O receipt；
  caller drop 后 delete 必须等待该 generation owner，不能在 workspace 文件删除后复活 journal。
- workspace eviction 一旦越过 idle proof 并开始不可逆 shutdown，就先提交 Closing。host 原子
  claim 一个 state-owned shared settlement，并由独立 task 驱动；eviction caller drop 只丢
  waiter，不会取消 shutdown 或丢失已消费的 debt。timeout、projection debt 或 pool/plugin
  failure 后 generation 保持
  Closing/Degraded，不能重新接受 control，也不能通过第二次 join 消耗并遗忘第一次 debt。
- framework command shutdown 由 mutex 原子 claim 的 shared future 持有；并发 phase-one/phase-two
  caller 观察同一稳定结果。framework shutdown 与 observer drain 共用一个总 deadline，panic、
  error 和 timeout 都进入 lifecycle receipt。
- TypeScript 只引用 ts-rs generated types。字段级 serde round-trip 与 source reachability
  测试防止重新引入 untyped JSON、numeric mode 或绕过 adapter 的关键路径。

## 取舍与影响

- 同步 `TaskRuntimeStore` 仍可供 blocking adapters、测试和明确的同步入口使用；禁止的是
  async production caller 直接执行文件 I/O。
- production inventory 覆盖 chat、background service、boot reconciler、revision、Subagent
  control、GUI/TUI/CLI/channel、worktree control 和 application/workspace shutdown；source
  contract 与 caller-abort/barrier 测试共同防止边界回退。
- 该边界解决 runtime starvation 和 wire drift，不改变 `events.jsonl` 的单一事实权威。
- `plan.json` / `run-state.json` refresh 在 journal commit 后降级的 typed outcome，以及
  Todo/Artifact/Completion 的 10k/100k 增量索引，是后续独立迭代；本 ADR 不扩大到这两个
  持久化语义问题。
- `echo-agent` examples 和 `echo-website` 不调用 EKO TaskRuntime/Tauri IPC，因此不需要同步
  代码或内容。

## 业界依据

- Tokio `spawn_blocking` 文档明确说明：任务开始后不能通过 abort 取消，runtime shutdown 会
  等待已启动的 blocking work。这支持“接受后完成 + owner 随 closure 持有”的设计。
- Tauri command 文档支持 async command 和 serde 参数/返回值；使用 Rust DTO 作为 wire
  source 可让桌面调用与生成 TypeScript 保持同一契约。
- Temporal 和 LangGraph 的 durable execution 设计都把持久事实提交与调用方异步等待分开，
  不以取消等待 future 代替持久 transaction 的明确结算。

参考：

- <https://docs.rs/tokio/latest/tokio/task/fn.spawn_blocking.html>
- <https://v2.tauri.app/develop/calling-rust/>
- <https://docs.temporal.io/workflow-execution>
- <https://docs.langchain.com/oss/python/langgraph/persistence>
