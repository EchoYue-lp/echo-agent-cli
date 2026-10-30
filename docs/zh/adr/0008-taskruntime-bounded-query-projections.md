# ADR 0008: TaskRuntime 有界查询投影

## 状态

Accepted（已采纳）

有限 GUI 历史查询适配已在任务列表分支实现；Rust、真实生成合同、高数据量与原生资格分别记录，不能从源码推定通过。


## 背景

TaskRuntime 已经以 `events.jsonl` 作为唯一事实权威，并通过 framework 的
`FileEventJournal`、`FileCheckpointStore` 和 `CheckpointedReducer` 维护可丢弃的
`checkpoint.json`。但是 Todo、Artifact 和 Requirement/Evidence completion 查询仍从
sequence 0 读取完整 journal；Todo 还会为每个 PlanTask 重复扫描事件。GUI 的真实查询延迟
因此随 10k/100k 历史线性增长，同时 `CommittedProjectionDegraded` 在不同写路径上被解释为
普通失败、静默成功或无 sequence 的特殊结果。

这不是 framework 缺少 journal/checkpoint 原语，而是 EKO 读模型绕过了已有权威。

## 参考实现

- [EventSourcingDB: Snapshots and Performance](https://docs.eventsourcingdb.io/best-practices/snapshots-and-performance/)
  建议从最近 snapshot 恢复状态，只增量应用 snapshot 之后的事件；snapshot 应低于 domain
  event 频率并按性能要求选择 interval，必要时异步生成。它是可重建加速层，不能替代事件历史。
- [EventSourcingDB: Read-Model Consistency and Lag](https://docs.eventsourcingdb.io/best-practices/read-model-consistency-and-lag/)
  建议用最后应用的事件位置度量 read-model lag，通过 checkpoint 和 lower-bound/suffix 恢复，
  并区分 write acceptance 与可能滞后的 read model。
- framework `CheckpointedReducer` 已实现相同模式：journal-first append、连续 sequence、checkpoint
  恢复、suffix replay 和 checkpoint 损坏后的重建。

跨实现的共同约束是：append-only log 决定“是否提交”，projection/checkpoint 只决定“读模型
是否新鲜”；二者不能使用同一个失败布尔值表达。

## 候选方案

### 方案 A：继续按请求全量扫描

实现最少，但 100k 历史会持续放大 GUI 延迟，且 Todo 的按 task 重扫会形成
`O(events * tasks)`。拒绝。

### 方案 B：新增 SQLite/read-model store

可以建立查询索引，但会引入第二套持久化权威和迁移/一致性问题，并违反 EKO 文件持久化边界。
拒绝。

### 方案 C：有界热 checkpoint + 增量 history read model

Todo display metadata、最新 Summary 和 completion evidence 是有界热状态，继续折叠到已有
`EventFoldState`。Artifact/Review 是可无限增长的历史，不能放入每次 mutation 都完整序列化的
snapshot，否则持续 append 会形成 `O(N²)` 写放大。它们改为由同一个 `RunAuthority` 增量维护的
可丢弃 read-model segments：Artifact 单独一段，Review 按安全编码的 stable task key 分段；共享
cursor 只记录已经从 journal 完整投影到所有相关 segment 的 source sequence。采用。

## 决策

1. framework 继续独占 journal sequence、batch commit、suffix replay 和 checkpoint recovery。
2. EKO `EventFoldState` 只保存 Todo、latest Summary、Completion Gate 等有界热状态；不保存
   Artifact/Review 全历史。
3. Artifact 使用 `artifact-history.jsonl`；Review 使用 `review-history/<safe-task-key>.jsonl`。
   每条记录携带 source journal sequence，没有独立 sequence、mutation API 或提交语义。
4. `history-cursor.json` 只在本批所有相关 segment `sync_data` 成功后原子推进。partial crash
   重放同一 journal suffix，并按 source sequence 去重。每个 segment 有 O(1) companion metadata，
   按 authoritative append batch 追加 cumulative count、最后 relevant source sequence、batch
   count 和增量 SHA-256 hash-chain frame；每个 frame 的链值是
   `H(previous_hash || persisted_batch_bytes)`，不是按事件重写增长 snapshot。发布顺序是 segment、
   metadata frame、global cursor。查询按 frame 流式重算并核对，因此合法 JSONL 前缀截断也会触发从
   `events.jsonl` 全量或按 task targeted 重建，而不是静默丢 facts。
5. Todo、Summary 和 Completion Gate 从 reducer 一致快照读取；Artifact 查询只读取 Artifact
   segment，Review 查询直接下推 task ID，只读取该 task segment。正常复杂度是
   `O(result + bounded suffix)`。
6. checkpoint 持有 EKO query projection schema。旧 schema checkpoint 在 recovery 时被逻辑
   忽略，从 journal 重建；物理删除和原子修复只做 best-effort，`PermissionDenied` 不阻断
   journal-derived 查询。`events.jsonl` 永远是唯一恢复事实源。
7. append 成功后的投影刷新使用 typed `ProjectionCommitReceipt`：
   `Durable { seq }` 或 `CommittedProjectionDegraded { seq, detail }`。只有 append 未提交才返回
   `Err`，避免重试一个已经提交的事件。所有会发布 `plan.json/run-state.json` 的 mutation
   都经过单事件或 batch helper。journal durability、checkpoint、history segment 或
   `plan.json/run-state.json` 任一派生发布失败，都返回 committed-but-degraded，不能把已提交
   误报失败，也不能把未提交误报成功。

## 影响与取舍

- 正常查询成本从 journal 长度解耦，主要与返回的 Todo/Artifact/Evidence 数量相关。
- checkpoint 只保存有界热查询字段；这些字段可删除重建，不获得写权限。
- Artifact/Review 历史本身就是查询结果，读取它们仍与返回条目数线性相关，这是预期成本。
- 增量 segment 是同一 journal authority 管理的 read model，不是 Store，也不能反向恢复
  `events.jsonl`。不引入 SQLite，因为本地文件 segment 已能按查询维度隔离扫描，且 SQLite 会
  新增不必要的持久化机制。
- 首次打开旧/损坏 checkpoint 需要一次完整 replay；之后重新进入有界路径。
- valid-but-behind checkpoint 在可写 open recovery 中一次推进；只读目录可能使 checkpoint 或
  segment 修复持续降级，查询仍从 journal 返回正确结果；同一进程按 journal head 使用有界
  per-task LRU 缓存 degraded Review fallback（最多 8 tasks、合计 10k records），避免 A/B/A
  查询重复完整 replay。单 task 超过上限时不缓存，避免结果与 cache 双份驻留，也不把常态全历史
  放回 checkpoint；
  下一次启动仍可再次尝试物理修复。
- 10k/100k 非 ignored 测试直接调用生产 Todo、Artifact、Review、Summary 和 Completion
  查询；history-heavy 门覆盖 100k other-task reviews + 1 target review、100k artifacts、跨规模
  后半程 append latency、segment scan/新增字节计数、文件大小和 restart，并另覆盖 partial
  crash、segment 缺失、合法 JSONL 前缀截断、空文件、损坏自愈、
  readonly fallback 与并发增量 append。

## 分层边界

- 通用机制（`echo-agent`）：journal、checkpoint、reducer、sequence、batch 和 suffix replay。
- EKO 产品策略（`echo-agent-cli`）：Todo、Artifact、Review、Requirement/Evidence 和 GUI
  completion 形状，以及按 query dimension 选择 read-model segment。
- 适配边界：`RunAuthority` 把 `RuntimeJournalEvent` 无损折叠为 EKO `EventFoldState` 和可丢弃
  history segments；它不重新实现 journal、DAG、task 状态机、sequence 或 mutation authority。

## GUI 运行与事件的有限读取

`list_task_runs` 按创建时间与 run_id 降序返回至多20个原 TaskRun。调用者携带的不透明 continuation 绑定原 Workspace/product-data generation、规范化状态筛选、页大小与排序位置，不建立查询会话或持久化状态。正式任务可见性与原会话 latest-task 查询共用 provenance/已提交计划判定，不凭诊断 route 字符串推导。

新读路径只保留一页加一个前瞻 header，但仍枚举 run ID 并恢复原 authority，不宣称常量扫描成本或已有索引。候选读取失败必须反馈，不能静默省略后冒完整成功页。分页读取实时事实，不是跨请求事务快照；状态/计划变化及新运行可能需要显式刷新。

原事件 IPC 复用 `query_events_bounded`，返回至多256条事件、无精度损失的字符串 next_after_sequence 与 has_more。缺口、非法位置和恢复失败仍是错误；前端保留事件数量不是传输上限，尚未读取的尾部继续显示不完整。journal、写入接受、操作准入和物理结算不变。暂停/取消/继续/重试展示原类型化回执；no-op 或已接受请求不等于执行完成。

参考 [GitHub 官方 REST 分页](https://docs.github.com/en/rest/using-the-rest-api/using-pagination-in-the-rest-api)的服务端有界响应和明确续页模式；EKO沿原本机IPC复用，不照搬远端服务限制。

历史检查在同一个 Zustand store 中至多额外保留一份呈现快照，不替换当前聊天的 activeRun、任务数量、失败提示或消息归属。任务面板操作携带实际显示的 run_id；冲突提示的 Continue 使用原准入 Workspace/run 与类型化回执，不读取选中历史的状态来判断目标。迟到结果不能清除后继提示或覆盖另一个呈现快照。

仅有投影的孤儿目录仍按原 journal 权威规则视为非运行；有限库存不从投影推断一个丢失的 TaskRun，也不新增注册表。非 NotFound 的 I/O、损坏 journal 与恢复错误仍明确反馈。

### GUI 任务控制绑定捕获的代次

原任务面板的暂停、取消、继续、重试，以及 Goal、续行预算、任务图、恢复决定与验收跳过等变更，均连同工作空间和运行身份传递捕获的 `workspace_generation`。`AppState::workspace_control_for_generation` 在同一个原控制回执上校验该 token；命令继续使用该回执的运行时克隆，保留原 host 租约直到实际操作结束，不能先校验一个 host 再按工作空间 id 查另一个。旧项目根版本的迟到动作因此不能修改替代代次。Continue 使用原输入准入捕获的范围，详情操作使用原渲染目标；这是目标身份约束，不是 Agent 权限门控。

显式历史选择即使与当前聊天运行相同，也保留唯一一份有界历史快照；后续聊天运行不能替换已选详情。详情和事件请求代次只结算各自的忙碌标记，跟随聊天或选择另一历史会转交对应标记，旧回调不能清除后继请求的忙碌状态。

### 持久化排队 Steer 替换

排队 Steer 编辑属于应用层对现有 ConversationInput journal 的消费。替换复用既有 `Persisted` fact，保持 input id，使用下一个 identity revision 和新的 payload hash，并继续校验原队列 revision。投影保留原队列位置和 first sequence。只有没有 active attempt 且仍可派发的输入允许替换；过期或正在执行的 identity 返回类型化校验错误。不新增第二个队列存储或 CRUD 权威。

## 当前只读查询与可选修复（2026-10-30）

completion report 已从原 RunAuthority 取得 journal 校验后的同一 projection，项目绑定校验直接使用同一 projection 携带的原 journal binding；不得为相同校验再次打开带派生写副作用的 run-state reader。当前 checkpoint 与合法 journal 在只读目录中仍可查询，可选派生修复失败继续保留 degraded 诊断；损坏或不连续 journal、异项目 binding、authority closed 与真实耐久性欠账仍按原边界拒绝。此调整不增加旧业务 schema reader、第二 store 或 validator，也不把缺失 checkpoint 当事实权威。测试分别保留当前 schema 冷读、坏 cache 重建、损坏 journal 拒绝和连续查询不重复全量 replay；实际运行资格按精确源码与日志记录。内部查询适配不涉及 examples 或官网公开 API，故无需新增示例或产品宣称。
