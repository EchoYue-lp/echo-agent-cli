---
schema_version: 1
id: baseline.repository
kind: baseline
source_snapshot:
  base_revision: 87eba93c6e557dafda2178e5b4763d91230fbcd6
  content_digest: 87791a9dc0b2bb4484fff726fea064bd943e080f4f0e97b74d9d5821442c1e9a
inventory_closure: open
behavior_model_closure: open
map_refs: [map.eko-conversation-collaboration]
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
coverage: []
---

# EKO application semantic baseline

## 源码快照

基线绑定 `echo-agent-cli` 最新 `main` 的可恢复 revision 与当前非语义源码摘要。设计和 Plan 已在当前工作树中，后续业务差异必须通过 semantic-diff 刷新摘要和受影响对象。

## 仓库区域

所有当前顶层 Git 路径均唯一分类。应用核心、Rust surface、Tauri 壳和前端属于生产范围，其余文档、配置、测试与构建材料作为 supporting consumer。

## 能力图与边界

当前只建立 EKO Conversation collaboration 边界，覆盖会话 transcript、AgentRouter inbox/group、foreground turn、删除、GUI Side Conversation 入口和非 GUI surface 保持既有行为的边界。

## 覆盖网格

库存与行为模型保持 open，因此不宣称其它应用能力的八视角覆盖已经完成；本次高风险任务由 capability map、Behavior、Rule、Evidence 与后续差异验证闭合。

## 未知与缺口

Side Conversation 的 GUI/Tauri、app-core 和文件权威闭环已由全量工程门禁与独立 Review 验证；模型覆盖恢复、一级关系、级联删除、首轮 GUI 驱动/admission、稳定 prompt identity、轮询隔离、原子 metadata mutation、viewed marker、内部消息投影和非 GUI 无专用合同均有执行证据。

## 闭合结论

当前基线结构、路径分类和 Conversation collaboration 边界已验证，可作为后续增量依据；全仓其它能力的库存闭合与行为模型闭合仍为 open。
