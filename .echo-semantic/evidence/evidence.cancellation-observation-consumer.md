---
schema_version: 1
id: evidence.cancellation-observation-consumer
kind: evidence
observed_at: source:215699889c9c12b8702566e900205bc6e2f73d00e83993834c0688cafa6e7464
source_refs:
- echo-agent-app-core/src/agent_pool/admission.rs
- echo-agent-app-core/src/agent_pool/pool.rs
- echo-agent-app-core/src/analysis.rs
- echo-agent-app-core/src/chat_driver.rs
- echo-agent-app-core/src/conversation_input.rs
- echo-agent-app-core/src/foreground_turn.rs
- echo-agent-app-core/src/product_data_io.rs
- echo-agent-app-core/src/runtime.rs
- echo-agent-app-core/src/run_driver.rs
- echo-agent-app-core/src/tasks/task_runtime/executor/limits.rs
- echo-agent-app-core/src/tasks/task_runtime/types.rs
- src/cli/channels/input_pump.rs
- echo-agent-app-core/src/scheduler/runner.rs
- echo-agent-app-core/src/tasks/task_runtime/continuation.rs
- echo-agent-app-core/src/tasks/task_runtime/event_rebuild.rs
- echo-agent-app-core/src/tasks/task_runtime/store/runtime.rs
- echo-agent-app-core/src/tasks/task_runtime/store/tests.rs
- echo-agent-app-core/src/tasks/task_runtime/store/workspace.rs
- echo-agent-app-core/src/workspace/runtime.rs
- docs/supreme/specs/cancellation-observation-consumer/design.md
- docs/supreme/specs/cancellation-observation-consumer/plans/plan_01_consumer.md
supports:
- behavior.shared-turn-observation-retains-owner
- behavior.task-unknown-owner-admission-fence
- behavior.analysis-original-owner-settlement-retry
- rule.eko-framework-consumer-authorities
limitations:
- Only two selected app-core cases passed with zero warnings on public intermediate f0a; remaining strong cases, surface conditions and final consumer gates remain pending.
- Source passed independent review through the 65-file candidate; this checkpoint adds metadata only. Upstream scheduler occurrence settlement remains open.
- Canonical relative paths are restored against public intermediate framework f0a6cd1; this is not final framework-main consumer acceptance.
- No full workspace/GUI/remote/main delivery claim.
---

# Cancellation observation consumer

## 支持的结论

原69a1首审五类Important经原reader有限增量收束：typed joinloss、live before-start取消、analysis pre-effect rollback、真正tool完成后的持久写入/ack债务、原run锁递归、旧cleanup污染newowner以及monitor晚发布竞争。bb919源审不等于测试绿。新增aggregate原total flow/lease等待与Owner Drop/Strong回边已在候选源码中收束。原Root强持实际资源、子系统仅Weak，opaque guard保counter、full控制保Host服务；真实weakRoot/Host回归尚未执行。

## 来源与范围

原件只在task-local `.echo-semantic/local/cancellation-observation-consumer/`，不新增raw文档包。source69a1/0cc5/7db690e5/bb919各历史cut保留。`appcore-check-v5.run.json`真实exit0/219B/无编译warning、F582/C302前后同；之后已审源变不能冒当前matching。首次 `cargo test -p echo-agent-app-core --lib --no-default-features --locked physical_owner` 41279实际101、30test编译错误/2imports，0行为tests执行；test-only迁移后原命令已实际重跑：focused-owner-v2实际exit0，1pass/0fail/0ignore/1635filtered、1051B原日志、0warning，F582/C302与上游39清单前后无漂移。收据SHA48521f65ee5c75e2d18fe9bb5fdaf3de9009e605337b9d5aac6cafffe64a8d0a。该foundation切点之后新增product-data精确flow分区和aggregate真实反例测试；preaggregate反例已实际编译/运行，after实现尚未编/执行；不能把此1case推广为1636全suite或当前aggregate pass。不把编译错叫行为red，也不把源assert存在叫pass。formatter control证明state/tests大重排是原官方formatter必要输出，非业务扩大。前置框架B反例属于框架，未伪称CLI original API已执行red。

## 已知缺口

aggregate Design81c/Plancee已独审PASS并获Root实施准入；本次仅对其私有实际字段/Weak图/清理顺序作精确实现同步。真正preaggregate `aggregate_shutdown_drains_normal_io_then_reports_original_unknown_pool_owner`：原shutdown/pool等待路径未修改，编译0warning，行为exit101/1fail/0pass；它先释放并join原backend、确认pool真实归零，再报告原total等待失败，原raw/收据300562d18002f0136db4669302ad9cbb027621f5e69ab45e53a2f393c3f63687保持不改。它不是当前after通过证据。当前Root/Owner/资源束、Weak原索引、原flow/lease分区、typedError单根晚完成及真实IO债务重试强case已准备；完整source review与after测试待Root窗口。`cargo fmt --all`和check新cut实际0/0B，diffcheck0；为F腾盘已按授权官方clean仅C可再生target，原源码/日志/收据全部保。

原source53e阶段两个manifest为验证专用绝对path；随后已按Root授权恢复canonical relative paths，实际借public f0a中间checkpoint。最终F main消费者和适用门禁仍待，不是最终公开交付cut。`.sem`当前引用更新不等于strict快照或行为验收闭合。示例与website未新增产品接口/能力，当前只修既有内部生命周期，不需要对外新例子或站点内容。

## Aggregate 固定四项源码复审

原64file manifest f3b761cee4a649e9fbbf92b74abe7e7c70489bb0dca9c5397a02aa4a56c8babd 首尾无漂移，独立实现源审C0/I4/M0：noRuntime发布次序、panic消费丢Owner、真实FG pool token漏标、Analysis最终提交丢wake。该候选当时已按同原owner修四项并加入受控强例，但尚未编译/执行；后继源审与当前有限执行以本Evidence末尾checkpoint为准，不把其它静态断言当通过。原64/patch、原before-v1与全部失败历史保留。新源须精增量独审和Root资源窗口after；formatter/check新cut实际0。

## 独立源审与 public f0a 具名执行

cold增量d212/64及首编修正e2ea/64均由原独立reader源审C0/I0/M0。Root随后准入四处canonical path恢复；两manifest逐字等canonical-before，lock c5a1392不变，实际独立clean sibling F=f0a6cd1/tree9ee1e434。首编v1真实101/3errors/1warning/0行为case，原raw保。四处修正后的v2原同名aggregate case实际1pass/0fail/0ignore/1643filtered，编译仍有2个cron retain must_use警告，故不是零警告工程通过。收据75b0e97097e88558d61f797180242758b8e705bdc9ba3bb87411eca389338993；583F/303C/64前后无漂移，own进程组最终为空。

该case只执行真实pool lease、registered driver与ProductDataIoService跟踪的oneshot-held异步operation；没有外部OS进程或实际文件I/O副作用。late清理由fixture先release/join同一backend，再写匹配physical completion与私有pending状态，仅为该测试可信清理，不证明无source生产路径自动获得后继receipt。其它真实fs/完整Root/opaque/analysis strongcase仍各自待执行，不把此1pass推广。

编译警告触发原typed-retain直接caller闭包核查：cron切同observed driver，接住memory/pool结果并保ExecError观察至ReactError；TUI/GUI retry把原Option pool lease转移至同receipt owner，不随观察future提前Drop。既有scheduler测试只加真实scoped根；新字段级roundtrip强回归尚未执行。源码定向检索其余receipt_owner.retain生产调用均已承接Result，未将Vec/foreground普通retain混为一类。

上游真实未闭：public f0a原SchedulerRunner将任意callback Err写Failed terminal并释放CallbackOwnerGuard，shutdown还可重排OutcomeUnknown。Root已收到精路径，须由原F occurrence/journal权威适配；C不建第二scheduler或假pending future。当前3scene needs_review/3lens stale保留，该caller增量当时未重编；后继v4/cron有限结果见下节，仍不宣cron端到端/最终Fmain/CLI交付通过。原raw只tasklocal。

## 零警告有限 focused checkpoint

同65源freeze82f16及canonical相对path/lockc5a，借独立clean public F=f0a6cd1/tree9ee1e434：v4原aggregate强例和cron typed-roundtrip各实际1pass/0fail/0ignore/1644filtered，完整app-core libtest编译零warning；各583F/303C/65执行前后无漂移，原进程组最终为空。原v3因free5GiB硬线中断，仅0case/未完成编译，Node未终发官方run.json，外部monitor与158B原raw诚实保留，不冒行为red或通过。v4/cron原件只在task-local，摘要focused-checkpoint-receipt.json；这两case不证明OS/file effect或无source自动恢复，也不闭上游SchedulerRunner occurrence策略。剩余精确9强例/GUI与TUI条件/最终F main消费者/全门禁另待，Plan仍ready，三个场景/宽Finding保持未完成。
