---
schema_version: 1
id: evidence.eko-native-measurement-source-basis
kind: evidence
observed_at: source:513d810144fb2c3348549b4734c9ae9275edd7cfbe1c152419b76db135f871f3
source_refs:
- echo-agent-app-core/src/conversation_projection.rs
- docs/zh/adr/0057-native-messages-and-progress-policy.md
- echo-agent-app-core/src/prepared_turn.rs
- echo-agent-app-core/src/prepared_turn/provenance.rs
- echo-agent-app-core/src/tasks/task_runtime/history_projection.rs
- echo-agent-app-core/tests/support/performance_baseline.rs
- scripts/performance-baseline-report.py
- scripts/performance-baseline-runner.py
- scripts/performance-baseline.mjs
- scripts/test_performance_baseline_report.py
- scripts/test_performance_baseline_runner.py
- web-frontend/src/components/chat/ReplyForkDialog.tsx
- web-frontend/src/components/subagent/SubagentNativeEvent.tsx
- web-frontend/src/stores/subagentRunStore.ts
- docs/zh/adr/0045-managed-conversation-lifecycle.md
- docs/zh/adr/0055-project-workspaces.md
- docs/zh/adr/0060-subagent-native-observations.md
- docs/zh/guides/task-run-history.md
supports:
- rule.eko-framework-consumer-authorities
limitations:
- 仅14条consumer当前源码依据；不是原四Behavior、宽Rule、删除聚合或动态义务完成
- ordinary source观察，不新增evidence_type、scenario matched、after_revision或命令绿色结果
- observed_at采用CLIwriter已提供的官方source标识，采用时变化须经其正常工具重绑；Gov未计算或伪造完整source/task身份
- 原宽Evidence及所有unknown/needs_review保持，旧43/Python和8/Node仅历史同字节离线控制，不是当前性能或新源Rust/native通过
- 未执行Cargo、模型、Host、项目性能脚本、native、手机/SSH、完整矩阵、正式Git pin、mainCI或发布；不改工具/schema/exclude
---

# EKO原生历史资源与测量consumer的有界源码依据

## 来源与范围

本普通Evidence只支持同一Rule对14个具名consumer路径的当前source依据，不创建Map、
Behavior、Asset、boundary或运行owner，不把现有channel owner扩成其它路径的虚构权威。
CLI唯一writer仍拥有正常SID写入、source/task观察和最终校验；此处的source标识来自该writer
已真实报告的观察，不是Gov独立推算的全库digest。采用时若源变化，须正常重绑，不能改旧
after/status/matched或把本次source审查当official gate通过。

最新原失败是14高风险路径依据加1宽scope删除/动态/needs_review聚合，旧42不再当当前分母；
30旧授权WIP allow由writer原流程负责。本观察不负责扩大allow，更不清最后聚合。当前00仅
旧API回归stage接入；未开始的retention修复/新F组合及运行结果不被本证据提前完成。

## 支持的结论

| 路径 | 真实原权威／consumer边界 | 只支持的source结论与限制 |
| --- | --- | --- |
| `echo-agent-app-core/src/conversation_projection.rs` | 原Framework Message与ChatEventLog publication；SavedMessage只是UI DTO | project_assistant_blocks／native_progress_publication_events／project_native_history_publication派生块与历史资格；冲突/缺原件不能靠phase补终态。不证明新retention完整通过。 |
| `echo-agent-app-core/src/legacy_eko_history.rs` | 同一PreModelContextProjector来源接缝，原Framework restore/CAS仍权威 | 当前recovered_tool_origins明确返回空Unknown；legacy_candidates只供诊断。不以旧名字/schema/trace或今天registry签Builtin。 |
| `echo-agent-app-core/src/prepared_turn.rs` | 原输入准备producer、product-data/runtime lease与实际artifact reader | to_message沿真实InputResourceRef加Parts；copy_reply_fork_history_resources_with_receipts在复制副本成功后才发布messages，失败回传实际副本/reader_verified而非补成功。 |
| `echo-agent-app-core/src/prepared_turn/provenance.rs` | 同一PreparedUserTurn的typed关联与原Message/StoredMessage | input_resource_parts／migrate_row只处理已知ResourceLink/meta并核role/version/URI/call/ref/范围；范围哈希只定位，不是独立来源。未知普通文字不升级资源。 |
| `echo-agent-app-core/src/tasks/task_runtime/history_projection.rs` | TaskRuntime journal唯一历史；HistoryProjection是可重建读模型 | journal_identity/through_sequence与段metadata/hash校验、raw_input_snapshot从原event派生；Current/Degraded不签执行完成，也不是第二Goal/历史store。 |
| `echo-agent-app-core/tests/support/performance_baseline.rs` | 隔离测试fixture消费既有产品Store/Journal/Runtime | 固定绝对隔离根、显式Result/release与样本身份/ns字符串；不是生产运行owner或已完成测量。 |
| `scripts/performance-baseline-report.py` | 只读报告/原观测与证据文件consumer | 拒非法来源/argv/profile/oracle/SHA/counter/覆盖；有效但不完整仍exit2、非法exit1，不重算分位或宣布基准完成。 |
| `scripts/performance-baseline-runner.py` | 限定harness子进程与原source/lock/profile/binary/manifest | 固定synthetic owner保护输入，同字节冻结/恢复，保原退出/失败，watchdog非SLA；不启动Cargo/模型/外部服务，进程计数不当单操作吞吐。 |
| `scripts/performance-baseline.mjs` | 冻结／验证／统计collector消费原样本 | 定义5warmup+30hot/cold10/memory20≤50协议与合法规模/单位分组，执行核对由原runner/harness负责；N<100不报p99，状态/预览不充性能成功。 |
| `scripts/test_performance_baseline_report.py` | 合成证据的离线拒绝控制，不是产品负载 | 覆盖坏SHA/oracle/profile/argv/counter/覆盖等；源码中的测试或其旧通过只支持相应parser控制，不产生性能样本。 |
| `scripts/test_performance_baseline_runner.py` | 合成目录/子进程输入保护的离线控制 | 五项workspace控制仅核固定根、外来改动/重定位、bytes/symlink、不收养已有目录与canonical bytes；不声称child/watchdog/feature argv已执行，不代实际测量。 |
| `web-frontend/src/components/chat/ReplyForkDialog.tsx` | 原后端Fork preparation/receipt与conversation/workspace scope | ownsSource核页面/代际、submitted固定同一request重试；resourceNotice只展示后端typed receipt，关闭不删除已创建资源或猜完成。 |
| `web-frontend/src/components/subagent/SubagentNativeEvent.tsx` | 原canonical AgentEventEnvelope与Task/Subagent lifecycle | phase=final_answer候选不展示；reasoning独立、unknown原标签、原envelope详情保留；仅呈现不改任务状态。 |
| `web-frontend/src/stores/subagentRunStore.ts` | 原ExecEvent/TaskRuntime Note与原journal replay | 校验原run/conversation/execution/turn、保完整envelope/序号和live-replay去重；native Note不是外层Task lifecycle terminal，不新增执行owner。 |

## 决策与实际采用来源

ADR0045记录同一Message中的资源关联与reader/持久化边界；ADR0055记录目标Workspace/
Worktree、原请求和资源receipt；ADR0057记录phase/原生发布与旧来源Unknown边界；ADR0060
记录原TaskRuntime Note、envelope、UI投影及生命周期分开。有限历史guide保原journal与页游标
权威。跨仓Plan08规定单位、合法负载、样本协议及测量证据边界，不为脚本创建业务owner。

原采用包分别保留native-production-adoption、workspace资源接线manifest、Subagent正式采用
receipt及performance integration/source-readback。当前源码若与旧candidate不同，只依据当前
bytes与已读函数说明source角色，不把旧source审查/运行票跨hash续期。逐文件SHA、anchor与
原回执可回读列表由Gov ignored source-basis-matrix/inputs保存；这些材料不是新的运行权威。

performance六文件已核与原integration/source-readback同字节。原Python38 report＋5 runner
共43离线控制、Node8的自然0只保其旧输入和范围；72只是argv语法核对，未执行runner动作。
原清单仍自标measurement not_run，不能拿这些结果称当前benchmark、release性能、quiet或
Rust/native绿。未把旧command_results复制成新执行，也没有新增scenario_results或matched。

## 已知缺口

本证据不支持原四Behavior或宽Rule完成，不修改其needs_review/unknown；旧宽Evidence全文和
已有source准入四个matched场景保持原范围。最后的删除/动态聚合若仍失败，应保真实失败。
Root原Goal及Plan09/10、当前retention先RED后GREEN、最终锁/风险、SDK正式new Git pin/
source402、native/手机/SSH/mainCI/发布均另取真实后继结果。本次不运行Cargo、模型、Host或
项目脚本，不修工具/schema/exclude，不清原失败。


## 2026-10-29：退役后的前置路径导航维护

本节只维护结构化源码/消费者引用的导航，不重新观察或续期旧运行资格。原观察 `source:513d810144fb2c3348549b4734c9ae9275edd7cfbe1c152419b76db135f871f3`、上文历史原件、失败与限制、原 command/scenario/after revision 均保持原范围；本节不新增 matched、状态或 owner。

| 原历史路径 | 当前实现或退役决定导航 |
| --- | --- |
| `echo-agent-app-core/src/legacy_eko_history.rs` | `docs/zh/adr/0057-native-messages-and-progress-policy.md` |

旧路径身份与退出范围由既有退役资产/Finding和正式ADR0050/0057保留；源码历史可在 `d17f88614b626d32a6176491b3bc60c92a84287f` 及后继具名Git保存点回读。新导航只指向现存原接管实现或退出决定，不能把旧observed_at解释成这些路径当时已经运行通过。unknown/needs_review、原动态/native/风险缺口不清；本候选没有执行Cargo或官方新Task校验，采用与真实来源绑定由EKO唯一writer按正常新Task合同完成。

上文legacy getter返回Unknown的逐路径行属于原source513d历史观察；该模块现已按NoCompat退出，仅从本次前置导航清单撤去，不宣称旧来源恢复已由其它projector替代或通过。
