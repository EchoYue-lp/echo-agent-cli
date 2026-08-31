# ADR 0019：Unified Application Services Composition

## 状态

已采纳，2026-08-29。

## 背景

GUI、headless、TUI、CLI 和 soak 入口需要相同的 AgentRuntime、AppState、TaskRuntime、pool
和 shutdown owner；分别构造会产生配置、生命周期和 receipt 漂移。

## 决策

由 `ApplicationServices` 负责一次 composition，并把共享服务注入所有 surface。入口只做
transport/renderer 适配，不能建立第二 runtime、store、DAG、status reducer 或 publication
registry。统一 lifecycle owner 负责 bootstrap rollback、admission close、cancel 和 join。

## 影响

产品行为和错误回执在五个 surface 保持一致，测试和 soak 使用同一 composition；EKO policy
仍留在 app-core，framework 不被桌面产品生命周期污染。
