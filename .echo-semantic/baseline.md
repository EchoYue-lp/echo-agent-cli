---
schema_version: 1
id: baseline.repository
kind: baseline
source_snapshot:
  base_revision: d4c6a80767fb688c2597fc623f922882b8718644
  content_digest: caa0bf81fcda8d046037a8a1065adc31b7bb0652059b4e4c8e975d8831e26fc0
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

已建立 EKO Conversation collaboration 与最新 framework consumer settlement 两个边界。前者保留 GUI Side Conversation 的原始交付快照；后者覆盖 Background Review caller-owned 生命周期及最新框架下 Skill/Plugin、Managed conversation、workspace 与 Scheduler 的应用适配。

## 覆盖网格

库存与行为模型保持 open，因此不宣称全应用八视角覆盖已经完成；本次高风险任务由新增 capability map、Behavior、Rule、Evidence 与后续独立复审闭合。

## 未知与缺口

Side Conversation 的 GUI/Tauri、app-core 和文件权威闭合已补充当前快照故障注入及独立复审。本地全量 Rust、GUI 和前端门禁已有候选证据；framework PR #174 已 squash merge 至签名的 `1927a5fc`，其 Git tree 与已验证候选相同。CLI 远端 CI、CLI/main 交付和 Issue #38 关闭仍未完成。

## 闭合结论

当前基线结构与路径分类可作为后续增量依据；新增 consumer Behavior/Rule 的当前候选已独立复审，全仓其它能力的库存闭合与行为模型闭合仍为 open。
