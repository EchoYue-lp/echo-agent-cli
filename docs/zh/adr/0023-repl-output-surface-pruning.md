# ADR 0023：REPL Output Surface Pruning

## 背景

`echo-agent-app-core::output` 曾暴露一个并未被完整生产路径使用的 REPL/TUI facade，描述与
实际 handler reachability 不一致。

## 决策

删除没有真实调用点的共享 REPL renderer、format/theme 和 dead shim；保留实际 TUI/CLI 输出
路径以及需要的 typed projection。能力清单以注册的 handler 和生产调用路径为准。

## 影响

GUI/TUI/CLI/channel 不再看到虚假的命令或主题能力，输出 surface 只投影真实 Agent 结果，不
改变 framework 通用事件或 tool contract。
