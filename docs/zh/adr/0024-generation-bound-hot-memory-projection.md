# ADR 0024：Generation-bound Hot Memory Projection

## 状态

已采纳并实现，2026-08-29。

## 背景

primary、existing pool Agent 和 future pool Agent 需要看到同一份 layered memory snapshot，
而逐 Agent 刷新会产生 generation 漂移、重复 I/O 和 workspace 切换污染。

## 决策

每个 global/workspace scope 只有一个 `MemoryLayerManager` 和
`HotMemoryProjectionSource`。mutation 只读取一次 durable memory，生成 immutable snapshot，
在下一个 pre-model safe point 发布给所有目标，并用 generation-bound receipt 报告 degraded debt。

## 影响

新旧 Agent、GUI/TUI/CLI/channel 和 `/reflect` 使用同一 memory authority；投影失败不回滚已提交
事实，下一次 settlement 按相同 generation 修复。
