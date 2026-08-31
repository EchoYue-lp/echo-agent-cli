# ADR 0031：确定性 CommandCell Watch

## 状态

已采纳

## 背景

EKO 过去注册一个 `awaiter` Subagent，它唯一的工作是反复调用 framework `wait` 工具，直到一个
后台 command cell 进入终态；随后 EKO 还要重新读取 typed cell state，再写入
`AwaiterResultReady`。模型 summary 明确不是权威，但整条路径仍依赖 model、prompt、Subagent
attempt、provider 可用性与进程级 Subagent permit。

framework ADR 0025 现在已经在既有 `CommandCellRegistry` 权威上提供 retained deterministic
watcher。

## 决策

1. 删除 builtin `awaiter` definition，以及 `watch_cell` 中全部模型派发、provider summary、
   `BackgroundSubagentHandle` 和 Subagent admission。
2. `watch_cell` 在 EKO durable exact-owner read 之前取得 `CommandCellWatcher`，继续闭合原有
   snapshot/live-retention race。
3. EKO 保留 `CommandCellWatchReceipt`、generation 幂等、有界 active watch、exact interrupt、
   Ready/delivery/ack fact、boot repair、foreground steer、next-turn projection 与全部
   GUI/TUI/CLI/channel renderer。
   watch admission 与 tracker spawn 在同一个 runtime-state lock 内和 shutdown 线性化，shutdown
   join cut 之后不能再出现 observer。
4. `CommandCellWatchResult` 只包含 durable receipt 与投影后的 typed `BackgroundCellState`，不再有
   provider-derived status 或 summary。
5. interrupt watch 只取消 observer 意图，不停止 command。配置后的 framework watcher 会继续短轮询
   到真实终态，确保已经接受的结果交付不会丢失。
6. 当前开发期事件名统一为 `command_cell_watch_ready`、
   `command_cell_watch_delivery_started` 与 `command_cell_watch_acknowledged`，不保留 legacy alias。

## 影响

- primary Agent 可以在确定性 watch 运行时继续工作，不消耗模型 token，也不占用 Subagent capacity。
- cell phase、terminal cause、exit code、output 与 artifact state 从 launch 到 delivery 只有一个权威。
- EKO 继续拥有本地 conversation/workspace identity 与 durable UI delivery；这些产品策略不进入
  framework。
