# ADR 0046：EKO Plugin 代际失败时关闭

## 状态

已采纳

## 背景

最新框架 [ADR 0012](https://github.com/EchoYue-lp/echo-agent/blob/main/docs/adr/0012-immutable-plugin-preparation.md) 允许隔离单组件错误的 prepared Plugin set，并禁止在同一 Agent 上重新发布已撤回的旧代际。[ADR 0060](https://github.com/EchoYue-lp/echo-agent/blob/main/docs/adr/0060-plugin-lifecycle-reconcile-settlement.md) 保留清理债务，避免回调重叠。EKO 旧 reload 路径在发布新候选后尝试恢复旧 prepared generation；这会被新代际 fence 拒绝，还可能让产品投影谎称旧 Plugin 仍活跃。

## 候选方案

1. 重新发布旧不可变代际。拒绝：框架正确地把它判为 stale。
2. 扩展框架，从旧包内容合成新代际。当前应用任务拒绝：扩大通用合同，且已经执行的生命周期副作用仍不明确。
3. EKO 遇到任一准备 Error diagnostic 时整代拒绝；候选已发布但激活失败时，以新空 Plugin 代际退役。采用。

## 决策

- 框架拥有不可变准备、每个 Agent 的发布身份、组件清理收据和回调 reconciliation；EKO 拥有“全部组件有效”的更严格策略与产品投影编排。
- 准备阶段任一 Error diagnostic 都拒绝 EKO 整次 reload，即使框架认为健康兄弟组件仍 applicable；诊断对调用方可见。
- 候选发布后若 callback 激活失败，EKO 停用并关闭 callback、移除候选 monitor、按精确收据 unwire 框架组件、卸载应用组件、释放 MCP 名称、用新代际退役 AgentPool 缓存组件、清除 LSP/theme/style 投影并持久保存退役偏好。任何清理失败都明确报告，不称作成功回滚。
- Plugin 代际之外的用户/项目 Skill 继续可见；primary、已有 pooled 和未来 pooled Agent 都不应再保留已退役 Plugin descriptor 或 Subagent。

## 影响

- reload 失败后 Plugin 集合可能为空，直到用户修复包并再次 reload；这比谎称旧 callback 或 descriptor 已恢复更准确。
- 项目仍在开发期，不保留旧代际兼容路径。
- 框架 examples 与 `echo-website` 不消费 EKO reload 策略，无需更新。
