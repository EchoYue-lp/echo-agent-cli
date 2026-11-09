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
- 候选发布后若 callback 激活失败，EKO 停用并关闭 callback、移除候选 monitor、按精确收据 unwire 框架组件、卸载应用组件、释放 MCP 名称、用新代际退役 AgentPool 缓存组件、清除 theme/style 投影并持久保存退役偏好。任何清理失败都明确报告，不称作成功回滚。
- Plugin 代际之外的用户/项目 Skill 继续可见；primary、已有 pooled 和未来 pooled Agent 都不应再保留已退役 Plugin descriptor 或 Subagent。

## 影响

- reload 失败后 Plugin 集合可能为空，直到用户修复包并再次 reload；这比谎称旧 callback 或 descriptor 已恢复更准确。
- 项目仍在开发期，不保留旧代际兼容路径。
- 框架 examples 与 `echo-website` 不消费 EKO reload 策略，无需更新。

## 当前组件与冻结诊断

Framework 当前 PreparedPluginSet 只准备仍支持的组件，不再探测、读取或将插件目录中的旧 LSP sidecar 纳入身份。EKO 删除对应的 Error 豁免、专用摘要文案和插件校验中的额外探测，不为旧声明重建 reader。所有当前组件的 Error 均按本 ADR 原策略拒绝发布；ReloadSummary 只保留同一 prepared generation 的完整诊断和原级别，不根据实时文件改写诊断。

消费者回归使用当前 MCP 文档与合法兄弟组件验证摘要完整性、缓存失效及拒绝候选时原已发布代际保持。领域 RunCreated 绑定、原 PreparedPluginSet、activation/retire、foreground/Subagent/verification/resume 和清理收据的权威不变；插件版本仍来自原 manifest，代际编号或运行时间不充当版本。Source、focused、native 与正式主线资格继续分别报告。
