---
schema_version: 1
id: audit.cancellation-observation-data-durability
kind: audit
boundary_ref: boundary.eko-framework-consumer-settlement
lens: data_durability
freshness: stale
revision: 5cd9eff0b92d076699d04b07d4a8a1c19e3fcdd4
finding_refs: []
challenges:
  original-owner-observation-and-cleanup:
    revision: 5cd9eff0b92d076699d04b07d4a8a1c19e3fcdd4
    source_refs:
    - echo-agent-app-core/src/tasks/task_runtime/store/runtime.rs
    - echo-agent-app-core/src/analysis.rs
    evidence_refs:
    - evidence.cancellation-observation-consumer
---

# Cancellation observation data_durability

## 审查范围

原framework-consumer边界单一data_durability视角。实现者归档既有独立源审与当前未验范围，不冒新增独立review或完整examined。

## 已检查故障假设

观察被假terminal化、restart丢intent、原receipt提前释放、old cleanup污染newowner与aggregate丢根/死等。只此有限切片，不穷尽全项目。

## 实际实现路径与证据

challenge列真实直接源码，共用Evidence严格区分源审、历史check、当前cut尚无after行为tests。原五I闭合的有限源结论不自动覆盖新aggregate。

## 问题记录

aggregate同原Owner续接、精确flow/lease分区、Weak回边和typed错误持根已落当前候选；后继当前code已取得app-core libtest编译零warning及两具名case通过；其它假设/强例不借此变为examined。真正preaggregate行为101与原backend清理已实录。当前freshness保持stale，不以实现者静态检查替代独立审查。

## 残余风险

当前强tests、真实F公开依赖、完整workspace/GUI/远端交付未验；资源不足不视为pass。

## 未检查项

不覆盖其他能力、SDK/框架外部消费者、生产安装；不把未来准入或设计pass当工程pass。
