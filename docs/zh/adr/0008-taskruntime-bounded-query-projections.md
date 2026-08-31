# ADR 0008: TaskRuntime 有界查询投影

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
