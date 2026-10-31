# ADR 0062：当前CLI五项风险子集的精确来源与绑定

## 状态

已批准。当前 HookTest 单 cfg(test) 增量的 525f／274 精确绑定及 2026-10-31 两次 fresh 风险门禁实际结果以[原中文风险决定](../../supreme/specs/cancellation-observation-consumer/dependency-risk-decision.md)本日 HookTest 后继为准；原 f173／bc08 历史保留，本 ADR 五身份合同与原人类接受范围不变，不授未来输入。Source／final 和其它验收仍独立。

以下为原阶段状态，保留其当时语义：

> 状态：当前GUI3后d027／273精确收紧候选；正式采用和fresh actual/canonical扫描门禁尚未执行。

## 背景

本轮原人类接受六项完整风险身份并要求完成当前修复与验证。批准的独立终端入口退役移除了bincode/syntect消费闭包，剩余五项是原六项严格子集。原policy仍绑定3fb/856与d38f/321；直接改digest或把旧7659隔离票当当前新源码资格都不成立。

原321输入已从本地Git提交0ebd8984的实际blob恢复，按原collector算法重算d38f。当前指定GUI3后冻结273输入的摘要为`d027ea4999a8bcbdc8d6262d4cfdd64f4669565135547e2b0e540e0baa01af9e`，实际锁`399509eeb951c87d407ffd949c60ac4c5b7dcc460a189e078a25f8287f4e2a4f`、768包。真实source差异为新增5/删除53/修改58/未变210：新增current native/provenance/capture，删除CLI/TUI入口，修改当前消息、IM/cron、Fork/Workspace、任务原文/进度和工具六分类。

Root本次后继freeze `gui3-final-source-freeze.json` SHA为`20dc7b2019f78f2bfd5a3b8fbfcfbbedabab8d34b29e6afb13a8e5a3194fcb96`，292构建来源＋14元数据共306成员已核回。完整89c7暴露审查之后，prepared_turn的cfg(test)类型修正形成2dc13，chat_driver的cfg(test)host reader/layout正控和诊断形成6a17；这些原票与diff均保留，生产暴露不变仍实际重绑。

6a17→当前d027已经包含真实production收紧：两函数`validated_reply_fork_publication_events`、`validate_reply_fork_publication`删除原no-native成功早退。本次逐函数比较证明除删除/说明注释外，原publication selector、owner、FinalAnswer、真实Settled、正文、双序号/checksum及import/current-source错误传播保持。无native消息现经原selector None映射为Validation，在原Fork准入前拒绝；没有新增网络/工具/模型/notification/native平台调用。其它diff是cfg(test)重复created_at/String构造、三透明future转发及worktree重复测试去重，不是生产或预算修改。因这是实质production变化，本次没有沿6a票续期，实际重新复查完整consumer并仅绑定d027。

原owner GUI47 distinct/0fail/1 quietignored和current官方source0以独立回执保存，三项GUI3已包含在25组内，不重复计票，也不签发依赖审计通过。后继非Rust文档索引若不在collector输入中，不影响risk consumer的结论必须由实际两读确认；文档/source门禁仍单独完成。任何再后继source/依赖/native桥变化都超出本票，按下述规则拒绝/复核。

## 候选方案

1. 继续原六项旧绑定或修改gate绕过缺包：拒绝；当前锁已经没有bincode，源码/consumer也已改变。
2. 按当前warning数量自动接受，或只改consumer摘要：拒绝；会使少warning、changed settings或未知source自动继承接受。
3. 逐字段证明原接受集的严格子集，复查完整consumer变动和真实平台来源，只绑定该冻结输入，随后由正常正式gate验收：采用此候选。

## 决策与证据

accepted仅删除bincode，保留glib0.18.5/RUSTSEC-2024-0429 unsound、proc-macro-error1.0.4/RUSTSEC-2024-0370、zvariant5.13.1/RUSTSEC-2026-0370、zbus_names4.3.4/RUSTSEC-2026-0371、zvariant_derive5.14.0/RUSTSEC-2026-0372四项unmaintained；所有source/checksum/advisory/kind与原六项逐字段相等。不是用户新批准五项，不叫风险已修复。

GTK/glib和gtk3-macros/proc-macro-error由当前Tauri GUI的Linux/BSD闭包保留；桌面notification插件和发送IPC仍在，notify-rust default z在unix非macOS经zbus消费三项D-Bus身份。已按锁checksum验证11份本地crate归档，读取的manifest/关键source均匹配；新增/修改的consumer没有新增这些crate直接调用或锁依赖边，但不证明transitive native API不可达或安全。

根Cargo保留依赖参数不变，移除16项终端依赖、default从tui转gui；明确承认桌面入口现在默认启用。锁856→768，新增0/移除88、共同完整身份变化0；resolved边新增0/移除24。GLib的VariantStrIter unsound和通知维护风险继续真实存在，不因本机app-core批没运行GTK/D-Bus而取消接受合同或平台验证。

checker固定当前five并要求实际warning集合完整相等。少项、增加、重复、4/6基数、身份字段、settings、漏洞、yanked和scanner错误均拒绝；原六方法保留，追加缺warning/settings负控。前次官方raw的自然1/0漏洞/五warning、原真实缺项RED以及全部原失败继续保留，本次没有fresh scanner。隔离候选控制不签发正式gate0。

## 取舍与影响

这是EKO应用风险绑定和工具合同维护；不改变Framework公共API、依赖实现、版本、scanner、filter、ignore或全局Git/SSH，不向SDK/Framework继承。无新增产品运行时API或存储机制，examples和官网无新能力/性能宣传可同步；原测试脚本就是可执行合同入口。

源码输入和风险暴露分别判断：仅测试类型修正可说明生产暴露未变，但仍改变consumer摘要。任何273输入增删改都先拒绝并取证；Root转交新精确值后更新引用/反控，不能提前给后来源码续票。依赖身份或风险/范围新增则保持阻塞交Root，不能仅凭“原五”继续。

正式owner在原reader安全点正常预检采用；必要Rust验证完成、最终source稳定后，Root另排原actual-lock/canonical-origin gate，保scanner自然出口及精gate出口原件。原source阻塞、native/平台、mainCI、发布及整个Goal均独立，不由本ADR完成。


### 2026-10-29 当前绑定的真实门禁结果

前文“候选／待正式采用及 fresh 扫描”记录保留为本次审查准备阶段的原件，本后继段记录当前实际采用与验证，不改原人类六项接受原话、候选暴露复查、旧失败或完整身份。原六项减去退役 bincode1.3.3 的五身份严格子集已经由唯一 EKO writer 在正常当前 Task 下正式采用；policy SHA 为 `5cfcbd288da8ca059d70e4a9599dd13cce643ea685d81e4839dff898688732b6`。

原 actual-lock caller 于 2026-10-29 04:00:29＋08:00 启动，canonical-origin caller 于 04:02:07＋08:00 启动，均真实采用默认 fresh fetch。两个官方 scanner 自然退出均为 1，完整 raw 和 stderr 原样保留；两个独立精确 gate/caller 实际均退出 0。两报告均为零漏洞、4 项 unmaintained 和 1 项 unsound，完整五身份、类别和 settings 与当前原接受合同精确相同；这不是零 warning 审计、风险修复或新增人类五项接受。

两次调用前后当前 316 项源码／元数据无漂移，collector 的 273 输入两读均为 `d027ea4999a8bcbdc8d6262d4cfdd64f4669565135547e2b0e540e0baa01af9e`，实际构建锁仍为 `399509eeb951c87d407ffd949c60ac4c5b7dcc460a189e078a25f8287f4e2a4f`、768 包。canonical 沿原 Framework helper（SHA `498670e5a346eac600eb1540bd392ef68fb3b475af51c750e4eecd9a0a300667`）生成独立审计锁，SHA `f6f52503a2de104add23c857ebd776eb060ddc18dca49d9ff19fc0f34e9e6b6d`；没有覆盖实际构建锁或修改依赖。

官方数据库报告为 commit `40f5be6fb12ed93fc591bf4ab23b7d793b3f2239`、1381 条 advisory。实际 fetch 将原 DB 从 `99670cfc0bc574433034b75876a4b791ccaa2efb` 更新到该 commit，canonical 的后继 fetch 时间也实际推进。DB 前／间／后 tracked blob 与模式均逐项核回；原六份未跟踪 RUSTSEC-0000 草稿状态原样保留并分别记录，没有擅删、忽略它们或声称 clean DB。没有新增 no-fetch、ignore、filter、URL 或全局 Git 配置。

原 actual/canonical `.audit.json`、`.audit.stderr`、`.gate.json`、`.caller-run.json`，以及 DB 三个状态快照和实核汇总，均保存在 CLI 仓内 `.echo-semantic/local/eko-appcore-fixture-resume-20261029/fresh-exact5-gates-20261029/`。Root 的独立 raw／gate／输入复核保存在顶层 `.echo-semantic/local/controlled-goal-resume-20261029/root-cli-fresh5-gates-readback.json`。官方自然 1 与精确 gate 0 分列，不用旧 raw 或合同控制测试替代本次 fresh 执行。

本后继事实只覆盖当前精确 consumer／lock／完整五身份和设置。任何后继源码、锁、consumer、身份、advisory、漏洞或 settings 变化仍先拒绝并复核，不给未来来源永久签票，也不向 Framework／SDK 继承；Rust、文档、原生、全矩阵、SDK 来源、main CI、发布与完整 Goal 仍分别验收。此次只追加非 consumer 采用／扫描事实，不重复 Rust／GUI／前端验证。
