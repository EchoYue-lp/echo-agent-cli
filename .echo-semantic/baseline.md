---
schema_version: 1
id: baseline.repository
kind: baseline
source_snapshot:
  base_revision: 6b162b6ab2aa13c8262fd6424ff089a692df70b5
  content_digest: 621b3930bfef37174da8309c6fde5c06dd749fd2be9cb4d48fdaf1e8c63628b1
inventory_closure: open
behavior_model_closure: open
map_refs: [map.eko-conversation-collaboration, map.eko-framework-consumer-settlement]
regions:
  - { path: .agents, status: supporting }
  - { path: .cargo, status: supporting }
  - { path: .env.example, status: supporting }
  - { path: .github, status: supporting }
  - { path: .gitignore, status: supporting }
  - { path: AGENTS.md, status: supporting }
  - { path: CHANGELOG.md, status: supporting }
  - { path: Cargo.lock, status: supporting }
  - { path: Cargo.toml, status: supporting }
  - { path: README.md, status: supporting }
  - { path: build.rs, status: supporting }
  - { path: capabilities, status: supporting }
  - { path: config, status: supporting }
  - { path: design, status: supporting }
  - { path: docs, status: supporting }
  - { path: echo-agent-app-core, status: in_scope }
  - { path: examples, status: supporting }
  - { path: init.sh, status: supporting }
  - { path: rust-toolchain.toml, status: supporting }
  - { path: scripts, status: supporting }
  - { path: src, status: in_scope }
  - { path: src-tauri, status: in_scope }
  - { path: tauri.conf.json, status: supporting }
  - { path: tests, status: supporting }
  - { path: web-frontend, status: in_scope }
boundaries:
  - id: boundary.eko-conversation-collaboration
    map_ref: map.eko-conversation-collaboration
    risk: high
  - id: boundary.eko-framework-consumer-settlement
    map_ref: map.eko-framework-consumer-settlement
    risk: high
coverage: []
---

# EKO application semantic baseline

## 源码快照

基线绑定 `echo-agent-cli` 最新 `main` 的可恢复 revision 与当前非语义源码摘要。设计和 Plan 已在当前工作树中，后续业务差异必须通过 semantic-diff 刷新摘要和受影响对象。

## 仓库区域

所有当前顶层 Git 路径均唯一分类。应用核心、Rust surface、Tauri 壳和前端属于生产范围，其余文档、配置、测试与构建材料作为 supporting consumer。

## 能力图与边界

已建立 EKO Conversation collaboration 与最新 framework consumer settlement 两个边界。Side Conversation 与 framework consumer 的既有对象绑定可恢复的 main revision，本次 GUI Fork 及受影响 Map/Rule 绑定当前候选源码。后者覆盖 Background Review caller-owned 生命周期及最新框架下 Skill/Plugin、Managed conversation、workspace 与 Scheduler 的应用适配。

## 覆盖网格

库存与行为模型保持 open，因此不宣称全应用八视角覆盖已经完成；本次高风险任务由新增 capability map、Behavior、Rule、Evidence 与后续独立复审闭合。

## 未知与缺口

既有 Side Conversation 与 framework consumer 的验证范围保留在历史 Evidence 中，不以本次源码摘要冒充重新验收。本次 GUI Fork 由新的 Behavior/Evidence 记录按钮、完整回合快照、独立列表条目与迟到选择保护；本地候选门禁不等于远端合并或原生 Tauri 窗口验收。

## 闭合结论

当前基线结构与路径分类可作为后续增量依据；GUI Fork 的当前候选单独绑定测试证据，全仓其它能力的库存闭合与行为模型闭合仍为 open。
