---
schema_version: 1
id: evidence.cancellation-observation-consumer
kind: evidence
observed_at: source:a7013e117da5c45675da2608ebf1b085872fc576f98bb01edfee7f42cedc0c8f
source_refs:
- echo-agent-app-core/src/agent_pool/tests.rs
- echo-agent-app-core/src/state/reliability_contracts.rs
- echo-agent-app-core/src/tasks/task_runtime/store/journal.rs
- echo-agent-app-core/src/tasks/service.rs
- echo-agent-app-core/src/state/config.rs
- echo-agent-app-core/src/state/tests.rs
- echo-agent-app-core/src/tasks/task_runtime/executor/dispatch.rs
- echo-agent-app-core/src/tasks/task_runtime/executor/run.rs
- echo-agent-app-core/src/tasks/task_runtime/executor/tests.rs
- src/cli/repl.rs
- src/cli/channels.rs
- src/tauri/commands/task_runtime.rs
- src/tui/events.rs
- Cargo.toml
- Cargo.lock
- .github/workflows/rust-ci.yml
- .cargo/accepted-dependency-risks.json
- scripts/check_cli_dependency_risks.py
- scripts/test_cli_dependency_risks.py
- echo-agent-app-core/src/agent_router/tests.rs
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
- echo-agent-app-core/src/plugin_runtime/tests.rs
- echo-agent-app-core/src/tasks/task_runtime/subagent_control.rs
supports:
- behavior.shared-turn-observation-retains-owner
- behavior.task-unknown-owner-admission-fence
- behavior.analysis-original-owner-settlement-retry
- rule.eko-framework-consumer-authorities
limitations:
- Earlier public f0a/3ded focused and nine-case results remain historical; they are
  not final CF988/full acceptance.
- Prior CF988 cut qualified cron, six surface/control cases and both Clippy commands.
  The later shared preparation repair has two same-entry AFTER passes; current full
  workspace, Clippy and GUI qualification remain pending.
- Exact three-dependency human acceptance has a reviewed gate; both online scans with
  per-process HTTP/1.1 report scanner1/gate0. Original default-transport fetch failure is retained.
- No complete workspace/GUI/remote-main delivery or repository-wide closure is claimed.
---

# Cancellation observation consumer

## 支持的结论

当前 checkpoint（CF988）：三个不同 focused case 各真实 1pass/0fail、零编译 warning、运行前后无源码漂移；独立实现与原件复审 C0/I0/M0。合法 unknown 逐 run 保留而不阻断普通恢复，原 existing-run identity/Goal 与入口错配校验保持，canonical compose 在真实 driver 准入前接同一 root。它不代表新 cron、全量矩阵或主分支交付通过。下文旧 cut 是按 epoch 保存的历史记录，当前完成边界以本段和最后 checkpoint 为准。

历史切点记录：

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

## Scheduler 原 Handle / Root 有界增量

当前公开开发依赖为 clean `3dedb9a6834cd28a3718d9852023722a6d24f4dc`（tree69240813），不是最终F main。原`SchedulerState`同Mutex slot跨await借原Handle，所有Err保留、仅真实Ok清；原Root每次poll均重观同slot并将Err计入uncertain，普通独立工作仍真实drain。没有新scheduler、状态/协议、effect重放或watcher；无late-source仍明确Unknown。

两原module逻辑用例真实before各1fail（slot1421B45dfd8a7、aggregate1811B70fc8eac），不是首轮2fixture编译错误。after各1pass/0fail/0ignore/1646filtered、编译零warning，slot原raw608B9f57dfda/编译7m12s，aggregate425Bc235323e/暖编3.25s；各583F/303C/67前后零漂移、无signal/trunc、最终进程组空。原正常control新增的是等同一public run_once实际commit再断成功；Unknown/取消等待/原Root两个红oracle均保。源码和收据独立复核C0/I0/M0；执行只含真实framework callback/资源、原Root与独立normal异步flow，非OS进程或无source自动恢复证明。原before、首编错误和描述旁注只保tasklocal，不入Git。

本增量不借旧f0a/v4或这两个case关闭其余9强例、TUI/GUI条件、完整workspace、cron E2E、最终framework-main消费者及远端交付。Plan仍ready、三个宽场景needs_review、三个原lens仍stale，库存和行为模型仍open。共享现有Rule/Map记录此窄原slot生命周期；其它历史观察用精确Git5cd9保留原215699摘要切点，未改其结论。


## 原九强例的当前实际执行

在CLI `2203e5edeaa9ed28445ab704535636aadfd48589` / 公开framework开发checkpoint `3dedb9a6834cd28a3718d9852023722a6d24f4dc`，此前prepared原九用例已实跑：七条Cargo命令，首组三个完整libtest名通过`-- --exact`并串行，后六组各exact一例；共9个不同逻辑case、9pass/0fail/0ignore，编译零warning。先用热binary确认实际选中数，listing没有当成执行；没有重跑已绿Scheduler两例，也不双算隔离子harness。原analysis组实际30.78s，30秒drain策略未改；每组实际CF1226/C434（Rust/Cargo与正式docs）及434冻结清单前后逐hash无漂移、noSignal/noTrunc/noMonitorError、进程组最终空。

这九例分别覆盖原Root的no-runtime单飞发布、foreign join unwind显式资源保留、analysis最终发布唤醒、精确FG token与同key普通lease、typed error单独持根、cold delivery原cell绑定、ProductData嵌套普通操作、opaque guard的Host回边和原durable intent重启拒重放。结果及输入守卫由原独立reader有限复核C0/I0/M0；原raw/runner/manifest只在tasklocal，汇总`remaining9-current-receipt.json`摘要2092eb57b9d8c9a600909f85ca96708627037fb358be8a315d3d2e62d3176a0c。

范围仍受fixture限制：typed-error持根用例在同backend先release/join后由测试写匹配physical completion/私有pending事实，不证明production无source能自动获取晚收据；normal异步flow不等于外部OS effect。foreign future的内部locals可能已自行析构，catch只证明框架没有额外丢弃显式外层Owner/bundle，也不建立任意opaque guard无环承诺。cold delivery用例验证原生产绑定helper/cell，不是整个GUI投递E2E。重启用例验证现有durable intent拒重放，不以应用重启推断进程已停。

旧f0a与5cd9/2203提交材料中“九例待运行”属于当时事实；当前摘要不再列该名单为从未执行。源码/非语义摘要未改变，baseline不重复刷新，Plan仍ready、宽场景needs_review/原lens stale保持；最终framework main消费者、适用whole/TUI/GUI、CLI cron E2E和其它未覆盖B义务仍待，不以九例关闭整个Outcome。


## CF988 当前定向 checkpoint

原材料中 b1f3c278 内容摘要已用官方 source_digest_at_revision 精确重算为当前不可变 Git ccf700ad4bed0fe4c84a1b376af97236a5b06cc7 的源码身份；该历史证明不改写成当前 dirty 源码的行为通过。当前源码和三份 after 的 303 Rust/build 输入逐字段一致，新增双语 ADR 为文档同步。原件保持在 `.echo-semantic/local/cli-cancellation-observation-consumer/`；不提交 runner、snapshot、log、临时 lock view 或 probe。

身份 bug 的有效 before 是真实 canonical boot 已 recovered/resumed 后，由 fixed Attended/agent_task_plan adapter 与合法 Unattended/autonomous run 的身份冲突拒绝；最小修后同合法 fixture 实际调用 provider 一次并真 join。随后只切原 late/early binding：late 真实报原 lifecycle owner 在 driver admission 前不可用，early 同 fixture 1pass（1.65s），无新增 hook/accessor、sleep 或放宽 5s。更早 masked deadline 不借为 root 时序红绿。

混合恢复用例 1pass 覆盖 Running/Paused 两状态、普通两 run 恢复计数、protected journal/cell/guidance/interrupt 保持、错误 ack 拒绝和畸形 intent 报错；Paused/pending 扩展不追认为旧 before 已红。另 1pass 验证 canonical domain/route/attended/Goal 与原 trace、错误 active workspace/conversation/root 拒绝且 journal 不变；三类 negative 不是三个额外 tests。三个 focused top-level case 不叠加隔离子进程计数。

该历史切点的真实锁和CF988 helper registry-origin view均有3个denied warnings，原online fetch错误不作通过。后继人已明确保留版本并延期升级，具名cron和surface证据见末节；宽场景needs_review、三个lens stale与globalClosure未检查不变。

## Primary surface checkpoint, 2026-10-17

用户要求C无损迁入primary CLI：940tracked文件和13文件补丁逐hash一致，C仅备份。P真实apply_patch成功，pre-edit记录root=P/host=codex/两个允许目标对应；仅证明编辑准入，不是Stop或native GUI全验。

channel真实ForegroundControl→waiter→notifier→renderer case编译后1fail/0pass，exact run/execution观察丢失。GUI独立case经官方FileEventJournal持久pending intent→重开原Store→真实launch返回RecoveryBlocked→实际错误结算函数，1fail/0pass，foreground误清为Failed。GUI首轮private counter编译错误不是行为红，改用既有is_run_active后才到业务断言。两有效before均sourceDrift=[]/CF988clean不变，原bin、patch与receipt保留。

同原路径修复后单条Cargo命令exact选6case：两个same AFTER、GUI普通error、channel正常terminal/continuation prompt/planned-resume steer控制，实际6pass/0fail/0ignore，exit0、零warning/sourceDrift=[]/CF不变。独审15file diff d9dc99870d5a263f205ab0552b895358d343618466eb8c657f82c84cc3d65738为C0/I0/M0。4测试lint整理被独审确认为非语义增量，原结论继续有效；17file diff8784617790cc7380fad83dd76bd4d575727fa3f3f8797b9ec3f9dac3dbbdb3c2的原workspace/all-target/all-feature Clippy -D warnings重跑真实0/零warning。所有旧101及来源检查失败保原件，不改写通过。

这些验证在P冻结副本执行，940tracked字节/mode一致，四canonical相对Cargo路径不变，兄弟framework symlink实际指向clean CF9886502/treeaee79a54。官方metadata --all-features --locked --offline为0/stderr空，8framework+3vendor canonical来源全CF、锁8e951不变；主框架Exa未采用/修改。先前unsupported Cargo paths警告的尝试不作通过依据。副本不是P原生Cargo或CI。

P `.echo-semantic/local/cli-cancellation-observation-consumer/`保primary-channel-notifier-before-v2、primary-gui-launch-before-v2、primary-surface-after-controls、primary-alltarget-clippy/v2各log/run/receipt及independent/increment review。具名cron消费者另CF988真实1pass/0fail/0ignore并独审限定通过；正常/unknown两控制同一顶层case，测试owned async backend/permit，不是OS/pool/memory或无source晚恢复证明。takeover-cron-cf988-unfinished原件保原C执行身份。

## Three dependency risks accepted by the user

人已明确保持glib0.18.5、proc-macro-error1.0.4、bincode1.3.3并延期升级。dependency-risk-decision.md和finding.cli-upstream-dependency-risks绑定精版本/registry checksum/消费者。风险未修复，unfiltered真实锁/registry-origin扫描仍1/3warnings，online fetch失败另存。没有宽泛ignore或SDK/framework特例移植；精确接受门禁仍待验证，不冒audit0或CI通过。

## Exact risk gate impact and qualification

从已审dfd5750 checkpoint续接：scanner和canonical framework registry-origin helper仍是原权威；新增内容只把EKO人的精确接受决定接到现有linux-quality步骤。直接影响policy配置、薄Python调用/结果核对、反例测试、原CI审计步骤及开发说明；不改变Cargo依赖/feature、Rust运行时、公共合同、UI、存储、原编译/测试门禁或其它仓库。共享Finding/Rule记录策略，原三个stale审查与宽B场景状态保持。

新gate将advisory/kind/version/registry source/checksum与完整CLI锁字节、全部应用Rust消费源码/manifest/build/Cargo配置指纹共同核对；变动须重新评估，不自动更新接受清单。不复制advisory扫描器或media-origin解析器；没有网络错误回退绿灯。官方原始JSON、stderr、scannerExit与单独gate结果保存在task-local/CI临时目录，完整扫描输出同时进入CI日志。

首轮5个unittest方法真实pass（含多个反例子场景，不重复计数）。真实默认在线扫描v1 exit1因RustSec fetch I/O失败且未产生JSON；adapter同样gate failed，失败原件risk-gate-actual-online-v1.*保留，不能借为3warning扫描或在线通过。首轮误用task-scope与既有repository预检不符的验证exit1保留；后续须以既定scope核对，历史Git引用收据专项仍未闭。独立review、缓存/新origin实跑和最终CI结果另记，不由测试存在推断通过。

配置覆盖Important已修复并独立精增量复审C0/I0/M0；新版6个unittest方法实际通过，reader另查空HOME/CARGO_HOME合同及Plan schema4/四章节绑定通过。最终gate四核心文件hash和六文稿的审查身份保risk-gate-independent-review-v3.json。反例含真实adapter对project/home/相对home/坏symlink配置在scanner调用前拒绝；不以fake scanner测试冒RustSec执行。

actual-cached-v2与origin-cached-v3为缓存有限检查，各官方scanner1/三warnings/单独gate0。默认online-v1失败后系统git ls-remote能读官方最新HEAD479c888ce4401313bca61ba4aba2d0f6244c4a09；仅对两次后继扫描进程指定GIT_CONFIG_COUNT=1、GIT_CONFIG_KEY_0=http.version、GIT_CONFIG_VALUE_0=HTTP/1.1，actual-online-http1与origin-online-http1均实际wrapper0，noFetch=false，官方scanner1/三warnings/零漏洞/单独gate0，DB HEAD仍同官方remote。未关闭fetch、使用stale或改全局Git配置；该受控传输设置解决本次现场fetch失败，更底层传输协商原因未证明。保留原始失败，不宣默认传输已修或CI通过。

原panic类别Clippy(--workspace --lib --bins --all-features --locked及四-D)真实exit0、2m36s、sourceDrift=[]/CF988clean不变，峰值8441212928B/最小free17226854400B。既有all-target Clippy也已0；两者不替完整workspace/GUI条件/JSONL/精main CI。剩余矩阵继续运行，宽状态不闭合。

## Shared preparation exact-entry repair

完整32c6719/tree8126025相对main f57的83路径独审发现此前15/17增量外的唯一Important：prepare_chat_execution把reserve准入的RecoveryBlocked转String，经共享foreground成为Ok(Failed)。原whole-candidate-review-32c6719.json保存block，不借旧surface6关闭这条入口。相邻previous-driver等待没有另一个错误转换，未证明的等待假设不扩成finding。

真实BEFORE-v2：official FileEventJournal写原pending intent、drop/reopen Store、实际drive_foreground_chat_turn；原生产prefix与32c逐byte同。编译4m34、零warning，2case实际1普通stale-identity控制pass/1Gui pending-owner业务fail、0ignore/1650filtered，失败值正是Ok(Failed(...chat driver admission failed...physical settlement...))。首Gui失败后其它三surface不追认before已执行。原before首次编译因独审先发现ordinary缺原resource scope/真实reject ack等待而中断，0行为，不算red。原fullworkspace也在行为前为新Important中断(-2)，不算pass/red。两种中断、原件与输入/资源收据均保留。

唯一生产源chat_driver.rs的原private prepare/claim改传既有ChatTurnError/From<StoreError>，ordinary context不变；普通scope/identity验证不变。原ordinary fixture复用既有test_resource_scope并等真实wait_for_run_driver_idle，不加API/sleep。same AFTER真实2pass/0fail/0ignore/1650filtered，1.09s行为、5m10编译、零warning、sourceDrift=[]/CF988clean；pending case实际走完Gui/Tui/Cli/Channel四参数，但只计一个顶层case。断原run/state/journal不变、0model/0event与foreground保留；普通error仍Failed/settled/清busy。不证明原OS执行或无source自动恢复。

独审source增量C0/I0/M0、同Plan四章节绑定0，source c3fe62e6308fa0c2ac1130c978a1f4d532f246785b1f379889eead89fa755df3/diff81d159ea78c33619a0a47c2045bd506da5bb4b4e37eafcb0392e9f44d04a4c07；完整83路径review的唯一源码Important已闭，但完整矩阵/最终CI未冒通过。risk gate先真实因新Rust输入gate1/scanner未调用；独审确认三依赖无新生产消费，依据原用户迁移并继续的指令、精确三依赖决定及本次有限复评记录后才重绑77ae4b12...a201，lock/版本/来源/checksum不变，非自动refresh。后继扫描另记。

本地原件：shared-preparation-before/v2、shared-preparation-after的log/run/receipt、before-v2.patch/source、before-freeze与独立inode原bin副本、shared-preparation-independent-source-review及risk-input-rejection。原件仅task-local，不入Git。双语ADR0004同步；无公共接口/示例/官网新增行为，无需改示例或官网；既有examples仍由最终all-target链验证。宽状态和原五历史引用收据缺口不借本切片关闭。

重绑后的6个risk测试方法实际通过。shared-preparation-actual-online与shared-preparation-origin-online在相同单进程HTTP/1.1条件下分别真实wrapper0/scanner1/gate0、noFetch=false、精确三warnings/零漏洞；后者由同CF988 canonical helper新生成CLI真实锁的origin view。不是复用旧consumer扫描，不改写原default-transport失败或最终CI状态。

宽B场景、三个stale lens、完整workspace/GUI、JSONL与精main CI不借此关闭，Plan保持ready，全局库存/行为闭合open。无前端实现变化，不额外本地frontend矩阵，CI原frontend保留；私有lint整理不改变examples/官网说明，既有examples由all-target编译链覆盖，后续完整门禁仍适用。


## Full-gate failure repair checkpoint (CF988, focused only)

原完整 workspace 测试出现 43 条失败记录和两个不终结的 mock，未取得完整 summary；保留原冷运行原件，不把停止前日志当完整结果。保存的原始 binary 对 43 个 selector 实跑 3pass/40fail。后续夹具修复切点实跑 40pass/6fail；其中两个原收据释放反例确证生产 debt 丢失已准入 owner 来源，另有目录恢复、进展分类、held cancel 和伪进程死亡夹具问题，各按证据独立处理。真实完成后 I/O 故障的第一次 AFTER 为 4pass/1fail，不能抹去残余失败。修复后原 48 项组合取得 48pass/0fail，仍是此前源码切点；其日志实际含 fetch_update 一条弃用警告，先前零 warning 的表述已撤回，原日志不变。读取错误修复的首次 49 项也通过并含同一警告，最终仅用原有稳定 CAS API 等价修正饱和计数，不压制告警或提高 MSRV，再冻结同 49 项验证。

既有 debt 现在持有原 PhysicalOwnerIntent；未知来源不得当作未开始。唯一重试路径在原 run 锁内验证同 owner、执行原 finalizer、确认后才在锁外释放资源。错误 owner、重复 ack、真实执行前拒绝各有控制。目录完全缺失时，缓存权威仅重建派生 state/checkpoint；夹具保留这些派生文件后恢复原 journal，并断言原 journal bytes/identity 不变，不删除或提升投影为权威。异步 pool-before-memory 释放和恰好一次计数保持。

closed-watch 竞争现在先尊重已经存在的真实 settlement/debt；mock 在原取消 token 下发出真实 Cancelled 并结束原流。测试资源绑定原 test_resource_scope / ApplicationLifecycleOwner，不修改生产默认 root。held cancel 只由同一原 driver 在真实释放后结算。进展 predicate 仅排除私有 owner 元数据，保留业务恢复事件；原 BEFORE 52 帧/56 条 canonical 记录表明七个唯一 owner 均按 false→真实完成→true→同 owner ack 顺序推进，没有 TaskStarted、claim reset 或 plan revision，排除了未确认 fence 被清后重放的假设。真实子进程退出夹具另保 caller-abort 控制，两子场景只计一个顶层 case，重开原目录后模型调用为零。

另一个独立拥有的 I/O 变体在保存的 36aa binary 实际报 Failed 对 Cancelled 的债务目标断言失败。该 probe 只替换自己精确匹配的故障占位文件，明确不是只读或原样夹具；canonical seq8→9 同 owner 物理完成、无 terminal/ack。窄 byte-equivalence 只证明五个生产文件与当前前一切点完全相同，不泛推整个 binary。最小修复复用原 cancellation target；原 Err 读取失败兜底保持 Cancelled/Failed，原 Ok 未业务结算仍 Failed。新 canonical-completion 后读取错误测试包含三个分支，只计一项；没有声称它自身跑过 BEFORE。

最终本次冻结 diff f8368ac5ea29938f179c4133ea87eeea3b15b323766685934e2f7121866e9648 的原 48 + 新 1 项实际通过；完整日志、真实 counts、资源和输入 guard 以 task-local read-error-combined-49-after-v3 原件为准。两位 source/必要性 reviewer C0/I0/M0；源级结论不代替运行。14 个源文件只包含上述必要修复与原夹具，raw 日志不入 Git。三依赖改变输入先 gate1/scanner=null；独立闭包复评仍同三项原风险，绑定依据是原用户授权，不是协调 agent 的新 human approval。

当前 CF 仍冻结 988；切换已接受的新 framework revision、当前全量 Clippy/workspace/core-no-default/GUI、安装后 repository/task semantic 收据及精确 main CI 都未借 focused49 关闭。Map/Rule 保持 needs_review，三审计 stale，inventory/behavior closure open；不证明原生 GUI/OS 或无 source 的自动晚恢复。仓库历史 Git 引用 collector 的五处缺口归独立 echo-semantic 修复任务，绝不改写本仓库历史引用以绕过。示例/官网不新增公共产品行为，无需新增代码；既有 examples 仍进入最终 all-target 门禁，双语 ADR 同步本修复。


当前新 consumer274d3e12...fed14 的六项 adapter 方法真实通过；重绑前原 unit6 中1error为实际输入守卫拒绝，原件保留，重绑后同命令0。fresh actual/origin 两次在线官方完整扫描均 scanner1/精三项 warning/零 vulnerability，独立 gate0、noFetch=false，只给扫描进程 HTTP/1.1，不改变全局配置，也不宣称默认传输修复。原 raw 与扫描输出保留 task-local focused-checkpoint-actual/origin-online 原件，不入 Git。


## Historical source continuity (source recovery only)

正式源码读取引用中的5cd有6对象/17refs/13路径，8bd有3对象/28refs/19路径；前者仅任务分支可达，后者原本已进入main材料但其源码commit不在main谱系。5cd全部13blob均不在published main谱系596个commit的可达对象中；8bd仅manual asset四文件在6b局部同，Fork对象及完整source digest不同。因此不改历史observed_at/revision、不借局部相同声称重审或当前通过。c874/dfd/32c/629/e0正文短SHA不是本scope源码读取绑定，未为其新增保存引用。

已按README记录的最小边界，仅发布两个annotated refs/tags/echo-semantic-source/<完整commit>，精确目标5cd9eff0b92d076699d04b07d4a8a1c19e3fcdd4及8bd2a6f08117cde61037b583000c71ab820e73ce；未发布其它本地tag或Release。两旧tree及相对main所需blob闭包经有界检查，未发现新增公开raw/真实凭据或构建产物；目标均已有公开branch/PR源，旧workflow不匹配tag push。7个二进制blob为应用图标，未把检查描述为任意外部自动化的穷尽证明。

独立新建网络仓库只fetch远端main f57与这两个精确refs，无alternates、taskbranch、PR或本地object借用；tag object/peeled commit/tree/connectivity及九个正式对象32个唯一source身份（含bytes/mode/anchor）均一致。官方source_digest重读5cd为215699889c9c12b8702566e900205bc6e2f73d00e83993834c0688cafa6e7464，8bd为e310dee0cb79fe7ee6d4072248885201202320e426d217bcf97b3e5e600558a0。原件仅task-local cold-main-evidence-tags-proof。此项只证明历史source可取，三Audit仍stale/Behavior仍needs_review；安装版collector五错误仍归独立插件修复。最终真实main合入后须再仅用远端main+必要refs做冷验，并完成完整运行矩阵；当前未声称完成。


## CF6563 surface fixture repair (focused checkpoint)

在真实 CF6563 冻结切点执行原完整 `cargo test --workspace --all-features --locked`，实际101：core 顶层1647pass/0fail/9既有ignored，后续五个integration组通过，root CLI285pass/3fail/0ignore。child harness不重复计数，未到达的后继target/JSONL/doc不算通过。三个失败分别是Channel模型屏障超时、GUI原生命周期owner缺失、TUI源码字符串旧分隔符已删除。

先保持Channel原夹具缺owner，仅增加既有JoinHandle早退诊断模式；同case实际101，真实driver在模型屏障前返回foreground_turn Failed，消息明确为original application lifecycle resource owner unavailable before driver admission。这使定因不依赖timeout推测。两个success fixture最终复用公开ApplicationLifecycleOwner，admission前bind原store并持活到原driver真实结算，再同owner Shutdown；原continuation/steer/rootcancel/terminal/foreground断言与负例均保留，生产guard未改。

TUI改为调用真实send_to_agent的planned_resume分支，持有真实runtime/foreground/pool leases，原Goal/revision/hash/root和run IDs均不变；同conversation独立待处理input的完整frontier不变、仍Persisted、无ConversationInputReceipt，最终foreground释放。真实v2 run指出创建时必须配置continuation，故沿用既有核心allowDirect fixture的creation-time配置，不降低Completed断言。初版冻结同步错误产生两处未声明owner编译错误、零case；v2真实2pass/1fail；失败原件、各patch/manifest与独立inode binary均保留，未称为通过。

最终三文件diff26e984ecfd1f2ccf362a0f3b11c7e93d57f8b2eb82de446cdb74043ed85c145f实际3pass/0fail/0ignore/285root filtered、2.25s行为/2m07编译/零warning；guard sourceDrift=[]、CF6563clean且不变，peak5630750720/free9400909824。独审source/必要性与原精确三依赖暴露均C0/I0/M0；每个生产prefix与9819字节相同。raw仅task-local cf6563-three-fixture-*，不入Git。三项风险按原人类授权及本次有限复评后才重绑，未升级依赖或扩大豁免；新在线扫描及完整门禁另记。

本切片只改现有测试与记录，不新增产品架构、公共API、示例或官网行为，因此无需ADR/example/website源码变更；既有examples仍须进入最终all-target门禁。Plan继续ready，Map/Rule needs_review、三Audit stale、inventory/behavior closure open；不借focused3宣称完整workspace/GUI/nativeStop/main CI通过。


本次重绑后六项risk adapter方法真实通过。actual在线HTTPS两次fetch失败且JSON为零、origin当时未启动，独立curl/nativeGit连接同样超时；失败全部保留。随后仅各scanner进程将完整官方RustSec HTTPS repo URL改为同仓SSH传输，新actual与新生成CF6563 canonical origin两次官方在线扫描均wrapper0/scanner1、精原三项warning/零漏洞、独立gate0、noFetch=false/ignore=[]，DB f28da8c5d386e507fac79153655184056a20c054。未借SDK旧报告，不改全局Git/registry/lock/filter，不称依赖已修。原件cf6563-fixture-checkpoint-actual/origin-online-ssh及前两失败只留task-local。

完整cargo clippy --workspace --all-targets --all-features --locked -- -D warnings实际0，7m55s、零warning，guard sourceDrift=[]/CF6563clean不变、peak5742702592/free7926157312；含既有examples，未以新测试源为由跳过。此后仅新增文稿事实和允许的当前快照metadata，三个Rust源hash与实际focused3/all-targetClippy仍一致，不重跑相同三例。另一组panic Clippy、原完整workspace/core-no-default/GUI/nativeStop/精mainCI继续待完成。独审文稿Minor“遗漏两次新fetch失败”已用上段事实补齐，不改失败日志或宽闭合状态。


## Executor gate fixture admission and durable-interruption prerequisite

c2d981后的原完整workspace命令实际101：core1645pass/2fail/9既有ignored，166.41s，后续targets未执行。该core binary与此前1647pass的保存bin逐字相同。原同bin cancel+revision两选择器2pass/1.66s不算稳定复现或修复；加入相邻pause后的自然三例组合实际1pass/2fail/5.52s。原三journal的8claims不冒充8个mock已开始：失败cancel仅2完成、pause仅3完成，均Running，尚无cancel/pause请求。

只加三个原失败分支诊断，原1s/5s/gates/强断言不变；实际2pass/1fail/5.87s、零warning/drift、CF6563clean。pause原5s点order只有3个gated dispatch，returned4、共享admission active5、文件IO8全部可用、原driver finished=false/pending；未观察到提前错误，不推断全过程占用或永久泄露。Notify丢沿假设被实际Tokio1.53.1保证及单waiter/permit结构反证。两个4gate夹具在全进程5槽内存在等待环，此证据支持隔离fixture配额作用域，未调global容量或deadline。临时诊断源码已删除，源patch、真实bin、原journals/raw只保task-local。

最终仅两源文件：canonical execute_runtime_plan仍绑定原process global5，同一执行体通过既有framework shared_admission Arc配置复用，四gate fixture各用同capacity5实例；IO8、原gates、时长与final断言均不变，无第二DAG/store/validator/public API。两个中断夹具原5s内保4gates和mock4返回，再从同Store确认原前4task IDs均Completed且claim=None后才发中断；TaskCompleted只通知，Store仍为权威。该前提修复不改原框架“中断先持久化可胜过未提交Completed”的CAS合同。

冻结Gitdiff39ba794c4d371f10bf460eedb401ea16055aae6e645bbc91b15eabdce9734c98，两位源码/必要性及精三风险reader C0/I0/M0，四gate+三canonical wrapper普通/依赖/失败对照实际7pass/0fail/0ignore/1649core filtered，root0case/288filtered不计新增通过，3.18s行为/9m27编译/零warning、guard sourceDrift=[]/CF6563clean不变、peak6210084864/free6546939904。此为focused证据；原full红与前次Clippy只保原切点，不借本次关闭完整workspace/Clippy/noDefault/GUI/nativeStop/mainCI。新consumer28312e...ecb2仅在上述有限复评与实际通过后重绑，原精三项风险与lock不变，新扫描另记。

设计仅更新Status/引言，四个Plan绑定章节与宽needs_review/stale/open状态不变；没有新产品架构或公共行为，不需新ADR/example/website源码，既有examples仍由最终all-target链覆盖。私有helper只转交原framework资源作用域，无迁移残留或并行执行权威。


此28312e新consumer的六项adapter方法真实通过（cf6563-core-admission-risk-unit，6/0）；新actual和新生成CF6563 canonical origin在线扫描各wrapper0/scanner1、精原三项warning/零漏洞、独立gate0、noFetch=false/ignore=[]。仅扫描进程的完整官方RustSec HTTPS→同仓SSH传输，无全局配置、ignore或新风险身份，未借此前283以外consumer报告；DB版本及完整JSON以task-local cf6563-core-admission-actual/origin-online-ssh与fresh-online-results为准。原HTTPS失败、旧full红和窄after证据都保留分离。当前累计对main仍84路径，新增修复落在其中已有两路径，不新增公共接口、示例或官网行为；剩余完整门禁/原生Stop/精mainCI仍须完成。
