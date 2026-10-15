---
schema_version: 1
id: behavior.task-unknown-owner-admission-fence
kind: behavior
status: needs_review
expectation: human_confirmed
risk: high
primary_focus: data_durability
focus:
- state_authority
- time_lifecycle
- data_durability
- failure_concurrency
- contract_evidence
boundary: boundary.eko-framework-consumer-settlement
observed_at: source:215699889c9c12b8702566e900205bc6e2f73d00e83993834c0688cafa6e7464
code_refs:
- echo-agent-app-core/src/tasks/task_runtime/store/runtime.rs
- echo-agent-app-core/src/tasks/task_runtime/store/workspace.rs
- echo-agent-app-core/src/tasks/task_runtime/event_rebuild.rs
- echo-agent-app-core/src/scheduler/runner.rs
- echo-agent-app-core/src/tasks/task_runtime/store/tests.rs
rule_refs:
- rule.eko-framework-consumer-authorities
evidence_refs:
- evidence.cancellation-observation-consumer
finding_refs: []
---

# task-unknown-owner-admission-fence

## 重要承诺

原FileTaskShadow在可effect前写exact owner intent；未知保claim/generation，recover/boot/retry/driver准入拒重复，只有同owner真实physical terminal能清一次。

## 当前行为

typed/durable/同Entry retry路径已有有限源码审查；原physical_owner已1pass；其后具名aggregate与cron typed-roundtrip各1pass/零warning；其它新aggregate/flow强例未运行。aggregate Design增量已审，当前生产候选已落；旧opaque切点随全部test modules编译，不等于最新ownership实现已编译，独立新回归未运行。

## 期望行为

框架保唯一physical finality/原stream/claim CAS；EKO复用原owner资源。普通真实receipt字段与公共UI/wire不变，观察知识不得变terminal。

## 触发、结果与副作用

原共享turn/Task/analysis入口真实可达。effect之前持久准入、terminal之前核真实receipt；同run/execution/generation保留到最后决定。

## 失败、重试与恢复

Unknown不进Failed debt、不丢原receipt、不重执行。真实terminal IO债务可同identity幂等重写，stale owner不能清新generation；重启不从内存消失推物理关闭。

## 证据

共用evidence.cancellation-observation-consumer明确源审、历史check与当前cut未验after范围；needs_review不冒已验收。

## 裁决记录

用户已批准B；Root准入必要CLI typed/durable适配，不新增公共状态、协议或第二supervisor。aggregate实现源已有限独审通过、当前app-core libtest完整编译零warning；只两具名case实际通过，其余强例与最终消费者未完成。
