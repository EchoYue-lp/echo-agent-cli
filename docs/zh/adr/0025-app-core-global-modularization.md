# ADR 0025：App-Core Global Modularization

- 状态：R4 实施采纳
- 日期：2026-08-29
- 范围：`echo-agent-cli/echo-agent-app-core`

## 背景

app-core 的大聚合文件同时承载 state、TaskRuntime、router、chat log、pool、extension、
plugin 和 infra，导致 authority 边界难以审计，surface 也可能绕过 facade 直接依赖内部路径。

## 决策

按 authority 拆分物理模块，并由 `echo_agent_app_core::api` 提供 namespace-preserving facade。
CLI、TUI、Tauri、channel、examples 和 integration tests 统一从 facade 导入。framework 继续
持有通用 turn、DAG、journal、plugin preparation、tool 和 memory 原语；EKO 保留 workspace、
文件投影、review/worktree、pool policy 和产品生命周期。

## 影响

拆分不改变 wire、serde/TS binding、文件布局或五个 surface 行为；旧 aggregate、dead shim 和
第二 authority 删除。新增模块必须明确 owner、复用现有 framework authority 并通过适用 gates。
