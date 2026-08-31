# ADR 0020：Interactive Tool Control Authority

## 背景

GUI 的 `enable_tool`/`disable_tool` 曾更新未被执行路径读取的空 map，optimistic UI 因此会在
重新加载后失效。

## 决策

交互式工具控制进入共享 `ToolControl` authority，由 Agent invocation 在执行前读取。GUI、
TUI、CLI/JSONL 和 channel 使用同一 typed request/receipt；不存在 surface-local tool state 或
只更新 UI 的假成功。

## 影响

控制变更在新 invocation 的 tool visibility 中生效，并带 workspace generation 和权限范围。
框架通用 Tool registry 保持独立，EKO 只负责产品策略和投影。
