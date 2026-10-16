---
schema_version: 1
id: evidence.cancellation-observation-consumer
kind: evidence
observed_at: source:2d05eb516554057681106fcab2222c5f47c79ac6fda54ad6954c2ff130de6d13
source_refs:
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

独审source增量C0/I0/M0、同Plan四章节绑定0，source c3fe62e6308fa0c2ac1130c978a1f4d532f246785b1f379889eead89fa755df3/diff81d159ea78c33619a0a47c2045bd506da5bb4b4e37eafcb0392e9f44d04a4c07；完整83路径review的唯一源码Important已闭，但完整矩阵/最终CI未冒通过。risk gate先真实因新Rust输入gate1/scanner未调用；独审确认三依赖无新生产消费，人的明确准入与Decision记录后才重绑77ae4b12...a201，lock/版本/来源/checksum不变，非自动refresh。后继扫描另记。

本地原件：shared-preparation-before/v2、shared-preparation-after的log/run/receipt、before-v2.patch/source、before-freeze与独立inode原bin副本、shared-preparation-independent-source-review及risk-input-rejection。原件仅task-local，不入Git。双语ADR0004同步；无公共接口/示例/官网新增行为，无需改示例或官网；既有examples仍由最终all-target链验证。宽状态和原五历史引用收据缺口不借本切片关闭。

重绑后的6个risk测试方法实际通过。shared-preparation-actual-online与shared-preparation-origin-online在相同单进程HTTP/1.1条件下分别真实wrapper0/scanner1/gate0、noFetch=false、精确三warnings/零漏洞；后者由同CF988 canonical helper新生成CLI真实锁的origin view。不是复用旧consumer扫描，不改写原default-transport失败或最终CI状态。

宽B场景、三个stale lens、完整workspace/GUI、JSONL与精main CI不借此关闭，Plan保持ready，全局库存/行为闭合open。无前端实现变化，不额外本地frontend矩阵，CI原frontend保留；私有lint整理不改变examples/官网说明，既有examples由all-target编译链覆盖，后续完整门禁仍适用。
