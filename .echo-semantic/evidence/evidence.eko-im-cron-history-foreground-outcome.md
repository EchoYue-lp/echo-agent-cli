---
schema_version: 1
id: evidence.eko-im-cron-history-foreground-outcome
kind: evidence
observed_at: source:756e27c482fc74fb8f3a4e25e57c314d80e41ae8b24c6abb2e033d048544e728
source_refs:
- .cargo/accepted-dependency-risks.json
- AGENTS.md
- docs/supreme/specs/cancellation-observation-consumer/dependency-risk-decision.md
- docs/zh/architecture/runtime.md
- echo-agent-app-core/src/channel_runtime.rs
- echo-agent-app-core/src/channel_runtime/native_message_tests.rs
- echo-agent-app-core/src/chat_driver.rs
- echo-agent-app-core/src/foreground_turn.rs
- echo-agent-app-core/src/scheduler/runner.rs
- echo-agent-app-core/src/tasks/task_runtime/continuation.rs
- echo-agent-app-core/src/tasks/task_runtime/file_store.rs
- echo-agent-app-core/src/tasks/task_runtime/store/tests.rs
- web-frontend/src/components/layout/LeftSidebar.test.tsx
- web-frontend/src/components/layout/LeftSidebar.tsx
- web-frontend/src/components/layout/RightRail.tsx
- web-frontend/src/lib/taskRunPresentation.ts
- echo-agent-app-core/src/chat_event_log/event.rs
- echo-agent-app-core/src/chat_event_log/journal.rs
- src/tauri/commands/task_runtime.rs
- web-frontend/src/api/endpoints.ts
- web-frontend/src/stores/taskRuntimeStore.ts
- web-frontend/src/stores/conversationStore.ts
- docs/zh/adr/0053-im-cron-gui-control.md
- docs/zh/adr/0057-native-messages-and-progress-policy.md
supports:
- rule.eko-framework-consumer-authorities
- rule.conversation-collaboration-authorities
limitations:
- 仅当前CLI/base667任务源码与具名工程验证，不是repository/globalClosure或所有旧Finding闭合
- 原6项release/scale/performance ignored不当已运行；完整性能仍属独立原窗口
- 编译采用本地focused strip-none工程profile，非release/stock/performance口径
- 当前51722仍为旧App0a02；真人微信授权数据已受保护，未被本任务修改、覆盖或重放
- 新App与手机IM/cron原生重开验收、模型成功、mainCI和发布仍待独立实际结果
---

# IM/cron 历史与前台续接结果的当前工程证据

## 来源与范围

本次从CLI基准 `66760c3f5b950843a7038bc7fd95e48a4850624a` 的干净切点开始；当前普通源码观察由未修改的官方 `verify_semantic.source_digest` 计算，见上方 observed_at。Rust/构建consumer由原正式collector连续两读绑定 `7441191402e2350ea32fdf72052c0a5f68c3ce0431f326608b37a3988b54bf74`／274，框架使用当前冻结 `340993201cc937fc8e4783b66be399ef9f14e3d5`，不改变框架或SDK公开API。

ConversationStore仍是transcript权威，TaskRuntime原journal仍是Run／Goal权威，ChatEventLog仍拥有原native/input事实。应用FileStore的唯一UI可见性判断纳入 `channel:` 与 `cron:` 的原Run，即使无plan也不隐藏；普通GUI轻量会话原语义保留。侧栏复用原listRuns、RightRail、loadByRun，以真实scope/generation/run_id去重和导航；同会话不同Run保留，部分读取失败不把conversation与Run重复成两行。周期只读取显式有限Run页，隐藏暂停，回前台刷新；没有第二store或后台执行器。

续接launcher保留本surface原journal根，当前finite turn独立；不把TaskRun原根强套合法surface-resume。前台只在原受控completion已Stopped/Completed，并经原受监督operation读回同Run/scope/conversation/TaskRun根且RunCompleted时，沿同一foreground slot回传不持久呈现观察。原finite receipt、RunTurnFinished与journal不改，换Run、取消、结束、迟到与后继slot有反控；原物理idle、generation、债务和Observation守卫保留。内部ExecutionStatus/InputLifecycle留journal，手机不逐条收到wire诊断；失败／取消仍取原typed Terminal。

## 支持的结论

真人旧微信执行真实标识以原journal的 `eko-task-driver:0d282cf4-b5ec-4ff4-8b11-6b2f4bdb4eed` 为准。原网络失败后因续接journal根拒绝形成downstream_disconnect并最终Cancelled；本任务不改写该旧行或重放旧输入。原有ConversationStore两消息与TaskRun已经落盘，缺陷在历史可见性／刷新及surface结果映射。

修复前同实际selector的header、IM诊断、完整自动launcher及历史重开四项各自然101／实际1失败。修正根身份后，真实ForegroundTurnControl Channel用例先通过RunCompleted／model2／原首typed Failure／两turn／无伪取消及物理释放，再在末尾outer Failed≠Completed形成独立RED；该反例没有用未修根造成的断流顶替。

原IM测试混合journal oracle误把总事实当2条，实际应保5个typed InputLifecycle和2个ExecutionStatus。原101保留；后继按typed两组及逐完整envelope/hash/sequence/payload原样比较加强oracle，phone文本断言不动。新增物理反控曾漏Duration局部导入，compiler E0433／自然101原件保留；后继只补cfg(test)导入，未改变生产、断言、等待、stack或skip，该工程失败不当行为RED。

## 当前实际工程验证

所有详细原件仅在本工作区 `.echo-semantic/local/im-cron-history-fix-20261031-r01/` 可用。摘要不代替原件，不把旧切点程序签为新源码。该目录从未用作正式source_refs或复用候选。

| 验证对象 | 实际方法与结果 | 原件入口 |
| --- | --- | --- |
| 修复后原4项、r06前台续接和2个反控 | 每项实际1／自然0；原native由Cargo执行，其余在同新合格harness运行 | green-r07-native-r02-caller.json、final-744-*-caller.json |
| 7个完整默认相关组 | chat_driver41、native25、aggregate24、scheduler17、continuation15、store176pass＋6原ignore、foreground31，均自然0；合计329pass／0fail／6原性能ignore | final-744-related-behavior-results.json及各group原raw |
| 实际harness身份 | Rust1.99.1，channels libtest，j5/debug0/inc0，Root具体批准的focused DEV/TEST strip-none；SHA `5607b1eb1d30d34d4b769eaa891648a9321fcea07e221e73b4e736b15bff7384`，FP/profile/8源与系统loader已保全 | foreground-after-744-program-qualified.json |
| GUI Rust | 原 `cargo check -p echo-agent-cli --lib --no-default-features --features gui,channels --locked` 自然0，零编译warning | GUIcheck-caller.json／GUIcheck.log |
| 前端focused与类型/格式 | 实际42／0fail，tsc/lint/Prettier均0，最终源独审C0/I0/M0 | frontend-minor-fixed-review.json |
| 独立前端完整unit | 原891pass／4原skip、93files pass／1原skip，零失败；固定4SHA及494输入无漂移，非bundle链 | echo-website工作区 cli-fullunit-sidebar-20261101/result.json，SHA a725a24b9b8bc3a47401f4211cca48997bd1c09d83629109801f70c70e8e7aac |
| 依赖合同 | 原8控制0；默认fresh actual/canonical官方scanner各自然1、零漏洞、恰5原warning，独立精gate/caller各0；不是风险已修复或零warning审计 | risk-fresh-744/two-fresh-risk-gates-qualified.json |
| 清理与保护 | 所有阶段raw/FP/必要程序在target外；准确own target reader0后官方clean自然0，未清其它target/真实数据 | official-exact-target-clean-readback.json |

旧525f、017 source预审、cd10前态及各失败各自保留其原范围；当前scope只覆盖本任务16个产品／配置／文档路径及本普通Evidence／原Map的必要反向关联，不关闭全仓unknown、其它Finding或6项性能门槛。

## 已知缺口

中文runtime说明、风险决定及Top／CLI同一事实块已同步；Top当前规则为bed78，CLI规则为ea4de66d，旧f60只属历史。没有新公开示例调用，examples无需修改；官网产品文案未扩承诺，正式runtime镜像由所属作者在合格源码后继统一更新。本修复沿ADR0053／0057既有应用边界，未新造状态机或通用框架产品策略。

本Evidence不证明最新App已构建／安装，不证明真人手机或cron触发已在新程序完成，不把当前旧51722/0a02的首页可见当修复后验收。当前名字仍为isolated-runtime的数据根已含真人授权和会话，按真实用户数据保护，未回滚config、复制Auth、重新扫码或写旧journal。Root统一stage/commit后新App须绑定新的真实commit，再按原owner安全点接续并取得原生证据。
