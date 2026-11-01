# RuntimeTaskService 适配决策

## 项目工作区与真实位置

工作空间、项目与工程对应同一个 Workspace，任务/会话归属它，TaskRun/PlanTask 列表呈现执行记录。创建 Workspace 支持已有本地文件夹、新建本地文件夹和 SSH 远程项目，新建任务不创建目录。代码位置与本机 `Workspace.root` 产品数据分离；当前 manifest 必须显式记录 `metadata.project_location`。缺字段时拒读并保留原文件，不从旧 `project_root` 或产品数据 `root` 推断，也不自动迁移。位置和验证后的执行身份同属原 manifest/`project_root_revision`，参见 [ADR 0055](../adr/0055-project-workspaces.md)。

创建或打开工作空间后，原 `workspaceStore` 使用后端 registry 回执按 ID 更新列表，成功进入时与当前选择一起发布；同 ID 的新 generation 替换旧项。创建已落盘但进入失败时只更新列表，保留原选择和错误，不再次创建或切换。迟到回执仍受原切换代次约束，不覆盖后继列表或焦点。

项目树、编辑器、changes/diff、Git/worktree 和 Agent 文件/命令使用同一 host 的实际位置。GUI 请求捕获 workspace ID 与 generation，迟到响应不能覆盖另一个位置版本。目录选择器仍浏览本机目录；远程路径由项目连接验证，不能在本机 canonicalize。提供者未承诺修改时间时返回 `null`。保存保持原锁/preimage 与真实目标的比较替换边界；远端不支持时明确报告，写结果不确定时保留诊断，不自动重放。

六个非本机项目文件入口通过原 ProductDataIo 观察最多 30 秒；Unconfirmed 只表示本次等待结束，原物理工作和 host 占用继续保留。真正完成后才清原 operation id，迟到失败保留原失败收据。本机/global 路径不改等待方式。位置重绑和 host 关闭沿原 idle 门禁，普通焦点切换不改变后台任务的目标。该观察不替代 SSH 后端对远端终态与未知结果的证明。

完成结果的封装继续携带同一原 operation，直到取出确认或未观察丢弃。成功写入通道缓存不构成交付确认：缓存被丢弃仍保留未读取错误，已实际读取的业务错误正常返回调用方。

用户级规则、记忆、产品会话和 journal 留本机；项目 AGENTS、Skill 和 Subagent 来源读取实际项目。必需读取失败从原可失败的模型入口返回。fresh Subagent 在实际隔离 cwd 已知后准备不可变指令与项目信息，现有模型前投影接口在冷 Execute/reset 或恢复后发布同一份已读取事实，以原 marker 替换旧 checkpoint 投影；角色根提示词保持原权威。这些已准备的项目事实投影不再次执行文件 I/O，也不承担项目来源的错误准入判断。

主智能体每轮通过原 turn registration 携带已准备的项目事实；显式提供环境、没有 store/run 的嵌入调用也走同一入口。原 EKO projector 同时保留 Domain/TaskRuntime、turn policy、hot memory 和命令观察。并行的主智能体验证使用已有 execution ID 作为该独立调用的 turn ID，原根消息继续作为触发来源。fresh 默认及 Plugin Subagent 将自己的真实 checkout 事实与原 Domain-only renderer 组合；本机产品数据回执始终是带身份的资源 guard，不是命令工作目录。

应用内嵌配置与用户 hooks/Skills 保留本机文件与原本机执行器；项目 hooks/Skills 捕获项目文件与执行器，后续切换 host 执行器不改写已登记的来源。hooks 仍按内嵌、用户文件、项目文件的原顺序合并，并在同一个 UserConfig 槽位整体替换。项目读取失败会阻止初次加载，显式重载失败保留上次有效集合。自动文件监听只覆盖本机配置和本机项目；SSH 项目没有远端 notify/poller，使用既有显式重载读取同一远端目标。

项目终端捕获项目身份；显式本地/独立 SSH tab 保持各自目标。人手终端不占 Agent 执行槽、不要求 LLM 或 full-auto。原 child/reader 结算前保持 host I/O receipt；退出先关闭用户 PTY，再结算其依赖，认证 master 等 Agent/file 使用者结算后关闭。源码和前端测试不代替真实远程写入、原生 GUI 或平台能力验收。

SSH 项目创建使用密码掩码表单，不展示认证终端。界面先订阅原会话事件，再启动原 OpenSSH master；只有原进程请求密码后才通过既有写入 IPC 暂时提交一次，提交后清空，失败或未知不自动重发。密码不进入 argv、env、配置、日志、模型或持久化 store。未知主机指纹由用户核对，不能自动确认。

登录成功时清除尚未使用的口令并锁定密码框；修改凭据需先断开，不能用旧 master 的成功代替新密码验证。目录验证的界面观察限为 30 秒，超时显示未确认并解除界面忙碌；原物理验证仍保留，同一待完成请求的显式重试只继续观察。取消、关闭或换来源后的迟到结果不再发起创建。创建 IPC 一旦派发仍沿原创建/未知结算合同，不套用只读验证的重试方式。认证视图卸载后关闭失败的资源仍由应用持有，通过原应用退出路径重试结算，不假称关闭完成。

“测试连接”调用捕获目标的 `test_project_ssh`，只使用原 `ssh -O check` 证明登录，不以 prompt 或进程启动为成功依据，也不要求填写目录。创建/重连时才通过同一连接执行 `verify_project_ssh`：相对路径和 `~/` 从远程 HOME 解析，再校验 SFTP canonical path 与原命令工作目录。未登录、目录缺失或读取失败保留表单与原错误，不发布工作空间。参见 [ADR 0055](../adr/0055-project-workspaces.md)。

## GUI 输入接收与草稿归属

运行中的追加输入以原 `queue_chat_input` 返回的持久回执确认“已收到”，不等待模型、物理工具或 mailbox drain。frontier 刷新和 tracked-steer 观察仍由原输入服务与 attempt owner 完成。接收、drain、已进入上下文与终态是不同事实；接收不表示当前工具已停止或纠正已经执行。一次草稿在附件处理前固定 externalId、原 Workspace/generation、Conversation/new-chat epoch 与领域选择；准备后的文本和附件负载沿同一描述保留，重试不换身份、不偷换负载。Hook 的原 turn/view fence 与 GUI 的当前草稿、附件及挂载检查共同保护迟到回执：原持久事实保留，不能清新稿或投影到另一任务。显式领域仍按原 TaskRun 绑定，排队时不能自动改当前领域。

普通首条发送以原保存者返回的真实会话地址继续准入。迟到创建不覆盖后继会话或新草稿，发送方不在保存后重新读取当前焦点来路由输入。Started 的直接接受证明和本地 slash command 成功，与输入持久化回执分开；GUI 仅在原提交和当前草稿仍匹配时清稿。

首次创建期间的提交展示权在第一次等待前绑定原 Hook 代次和原准入对象；原保存者在选中真实会话前通知该对象，只有仍属于原代次的 null→真实 ID 变化可以迁移。Stop、卸载、换会话或新草稿后不能重新取得展示权。创建成功与后续列表刷新、界面观察异常分开处理，不因观察失败再次创建。`/clear` 的原 reset 可在后台结算原会话，但返回后只在原 Workspace、选择、加载代次和新稿 epoch 均未变化时清理当前投影；原当前会话 reset 失败后仍按既有行为清空。这些回调复用原会话生命周期，不新增第二份 store 或执行 owner。

## 必要原文的有界投影与可恢复暂停

TaskRuntime journal 保存完整 user steer，原活动 capsule 对最近四条只放短文本或
400字符预览及完整SHA引用，同时提供绑定原journal代际和序号前缀的完整来源入口。
主模型沿原read_artifact按需读取更早原文，不把近期预览当作全部历史。长原文共用 PreparedUserTurn/InputResourceRef 的落盘、
SHA和引用格式，内容寻址缓存缺失或损坏只从 journal 重建；不增加输入store或权限。
该 TaskRuntime 投影与上面的已准备项目事实是不同来源：其原文件读取通过既有
TaskRuntimeOperation 进行，任何新原文物化/校验失败必须沿required错误通道阻断。

原文仍在host产品数据目录，SSH项目的FileSystem/read_file不承担其读取。最终模型
请求构建后用真实snapshot的artifact配置核验，并要求该请求实际暴露read_artifact。
非空allowed_tools排除reader时暂停，不绕过或放宽上界；仅400字符预览不能代替全文。

框架仅用既有typed AgentFailure发出required_context_unavailable。EKO共享一个
machine-code分类helper：正式PlanTask经原exact-claim interruption及settlement CAS
暂停；普通GUI/IM经原RunTurnFinished和turn_lifecycle暂停为NeedsInput。原failure
原文、Goal和journal保留，不设置自动retry。修复来源后通过原expected-resume恢复，
不创建新Run、不增加retry；取消/旧claim/物理未结算仍保原优先级。参见
[ADR0048](../adr/0048-context-compression-product-policy.md)。

这只是当前有界引用和错误消费边界，最近四条不代表完整历史。普通Steer由主模型结合
历史理解并按需调整计划；原文、输入身份与必要上下文继续沿原journal和reader恢复，
不再要求独立意图模型或统一强制的有效要求/计划/验收更新流水线。

## AgentRouter 阶段交接

AgentRouter 直接使用公开的 typed
`echo_agent::delivery::DeliveryLedger<Journal, AgentAddress, AgentMessage>`。journal、
checkpoint、claim、cursor、settlement、FIFO、retry 和 retention 都是 framework 的 typed record；
活动路径不再有 app projection/reducer 或 wire conversion。AgentRouter 直接传递 framework
`DeliveryTransition`，再通过 `DeliveryLedger::apply_prepared_with` 提交已校验的 event；应用
callback 只负责物理 journal durability 与 reopen，EKO 不再定义一套 lifecycle command enum。
`AppState` 仍负责 EKO endpoint 校验、
live/cold runtime 选择、wake 调度、groups、retirement 和 surface policy。当前处于开发期，schema
切换后旧本地 inbox 数据直接丢弃。

状态：typed framework ledger 已成为唯一 delivery authority

## 背景

EKO 曾在应用层同时维护 DAG frontier、claim、retry、pause/cancel 和 descendant block，
而 `echo-agent` 已提供 revisioned Task graph 与相同的通用执行语义。两套权威会使 stale
claim 覆盖新 attempt、暂停消耗 retry、依赖阻塞长期写入 journal，也使 GUI/TUI 看到的
状态与真实调度状态不一致。

## 候选方案

1. 保留 EKO executor，只复用 framework 数据类型。迁移最小，但继续存在双状态机。
2. 把 EKO review、worktree、文件字段和 UI 状态全部下沉 framework。只有一个 executor，
   但会用本地个人助理策略污染通用框架。
3. framework 拥有通用 task service；EKO 提供薄 adapter 和产品策略。本项目采用此方案。

## 决策

`RuntimeTaskService` 是 production DAG 的唯一构造入口。分层如下：

- framework 通用机制：DAG 校验与 ready frontier、exact physical claim、revision safe point、
  retry/requeue、terminal settlement、pause/cancel disposition、derived dependency state。
- EKO 产品策略：review、worktree integration、文件 ownership、unattended preflight、run
  pause/cancel intent、GUI/channel 投影。
- adapter：无损转换 `PlanTask` extension，调用 framework pure transforms，并把 task 变化、
  EKO summary 和 claim-bound review 作为一个 `append_batch` 提交。

`events.jsonl` 是 EKO 权威。`plan.json`、run state 和 Todo 是可重建投影。`plan.json` 只保存
`PlanRevision` specification，run state 保存无损 framework `TaskStatus`；Todo 只能由两者投影，
不能反向驱动调度、completion 或 recovery。projection refresh
降级发生在 journal 已提交之后时，调用方保留 typed mutation outcome，后续读取自愈；
batch outcome unknown 则 fail closed。依赖失败只通过 `DagExecutionState` 派生到 Todo read
model，绝不写 descendant `Blocked` 事实。

EKO 的 `TaskExecutionSummary` 与 `SuggestedTask` 是产品持久化记录，包含 review、task kind 和
surface 字段，直接在 EKO 中保存；未接入的 `to_runtime_*`/`from_runtime_*` 结果转换 API 已删除。
`EkoConfig` 同样继续作为产品文件 schema。bootstrap 只从中选择 framework runtime 配置所需的
provider-neutral 字段，不再增加第二套配置转换包装；framework 调用方通过标准
`From<FrameworkConfig> for AgentConfig` 合同完成转换。
`TaskExecutionSummary.outcome` 与 `SubagentRun.outcome` 是 EKO 持久化 typed Subagent outcome
的唯一字段。framework 的 `SubagentResult` 仍是包含输出、耗时、usage 和 mode 的执行信封；EKO
不会再把它复制成第二个 result DTO。
`ConversationInputOutcome` 只保留为 EKO 的 wire 名称，Rust 直接使用 framework
`AgentSteerTurnOutcome`，不再有逐枚举的 outcome 转换。
`ChatSteerOutcome` 同样只保留为 GUI wire 名称，Rust 直接复用该 framework outcome。
`SubagentRun.usage` 直接保存 framework `ExecutionUsage`；`SubagentRunUsage` 只保留为
generated TypeScript wire 名称。executor 不再用 `TaskExecutionUsage` 包装 framework
`SubagentResult` 或 `TurnReceipt`，两类结果都通过自己的 `usage()` API 直接提供
`ExecutionUsage`。
EKO command receipt 同样直接别名 framework `SubagentCommandIdentity`，不再定义第二份
通用 identity 模型。
其 durable phase 同样别名 framework `SubagentCommandPhase`；EKO 只保留
`Pending/Accepted/Rejected/Settled` 这一层 surface status projection。
权限状态遵循同一 framework-native 规则：`ConfigState` 直接保存
`echo_agent::tools::permission::PermissionRule`。Tauri 通过 framework 的 `FromStr` API
解析 matcher、behavior 和 source 字符串，不再重建应用层 `PermissionRuleConfig`。
EKO 的完整任务规格使用 framework `TaskSpec::with_extension` 与 `TaskSpec::extension_as` 处理
typed `EkoTaskExtension`。只有部分字段的 `TaskPatch` 继续使用 JSON object，因为它表达的是
局部 patch，而不是完整 extension 值。跨越 EKO projection 边界时使用 Rust 标准
`TryFrom`/`TryInto`，不再暴露按来源命名的 `to_task_*` 或 `from_task_*` API。

TaskRuntime executor 中两类有限 primary-Agent turn（独立 run 的规划 turn，以及 PlanTask
回落到 primary Agent 的执行 turn）统一交给 framework `AgentTurnDriver`。framework 独占 raw stream 启动、
envelope sequence、exact terminal、typed failure、cancellation 和 provider-reported usage receipt；
EKO 只有一个 `EkoAgentTurnSink`，负责 `ExecEvent`、event-id usage、tool start/terminal、evidence
与 artifact 投影。流在没有 terminal event 时必须失败，不能再以“stream 已结束”推断成功。

RunTurn terminal 的应用权威位于 `task_runtime/turn_lifecycle.rs`。chat、background/cron 和
`create_complex_task` 的 owned driver 都先提交同一 `RunTurnFinished`，再由同一 service 决定
provider retry、budget、cancel、pause 或下一 turn。Owned driver 不调用 detached
`request_continue`，也不等待自己的 driver-idle；它通过 `await_owned_continue` 复用同一
eligibility、durable retry deadline、shutdown/cancel 和 command-cell wake source，并在
Deferred 期间继续持有原 registration，因此不存在 handoff 空窗或第二套 active state。
Lifecycle 的 journal 读写全部通过共享 `TaskRuntimeOperation`；pool/configuration 在
model 启动前被拒绝时也只调用该 service，不再在 chat driver 复制 finish/pause/cancel/cell
policy。

所有 RunTurn claim 的 run 必须在创建事务中显式配置 continuation。Provider-reported usage
按 envelope event id 写入 active RunTurn；未报告的 usage 不会被记为零成本事实。Primary
PlanTask 的 typed `AgentFailure` 作为 durable Subagent evidence 进入 exact-claim settlement；
retryable LLM failure 使用稳定 fingerprint requeue，并把 retry exhaustion 的 `Failed` /
`TimedOut` 分类显式交给 framework；direct timeout 和最终 exhaustion 都由 framework 在同一
CAS 中提交为 `TaskStatus::TimedOut`。EKO adapter 不再先结算 `Failed` 后改写状态，也不根据
product metadata 推断 terminal event。
Task-level Completed/Failed/TimedOut/Cancelled/Blocked live projection 只在 exact claim CAS
提交后发布；physical Subagent attempt 可以先发布自己的 terminal，但不得冒充 PlanTask terminal。

`create_complex_task` 创建的 attended 独立 run 会把当前 surface 的 `HumanLoopProvider` 作为
值传入 run-scoped pooled Agent，并保留已有 approval cache。Unattended run 不注入交互 provider，
因此 cron 或其它 unattended 独立 run 不会等待一个不存在的用户输入 owner。该差异是 EKO 产品策略，
不进入 framework turn driver。

暂停与取消遵循 `Cancelled > Paused`。EKO 不新增 task-level Paused UI 状态：active task 的
framework Paused 在文件 adapter 投影为 unclaimed Pending，Run 保持 Paused；resume 不增加
retry。取消先持久化 Run/Task Cancelled，再触发 driver token。

Resume 只有两个权威 intent，二者都携带从 journal projection 捕获的
`TaskRunResumeIdentity`：

- 已有 Plan 的普通 run 通过共享 `launch_planned_run_resume` 恢复同一
  `RuntimeTaskService` executor；GUI/channel 不自建 launcher。
- long-horizon chat 通过 `resume_and_claim_run_turn_expected` 在同一物理 batch
  提交 resume 事实与 `RunTurnStarted`。

identity 包含已 fold 的 journal sequence，因此 pause ABA、附件或 continuation 变化都会使
旧操作失效。CAS 未提交时只拒绝 driver registration，不得把当前 run 改写为
Failed/Cancelled；append 结果不确定时必须重开 journal 并对账。

RuntimeTaskService executor 和共享 planned-resume adapter 的异步文件 I/O 通过固定上限的
`TaskRuntimeOperation` 进入 `spawn_blocking`。进程 semaphore 只限制并发；每个 store 的
operation supervisor 持有 blocking handle、RunDriver、CommandCell observer、多阶段 Subagent
command 和 turn projector，调用方只等待 receipt。caller future Drop 不会 detach operation；
Application phase one 关闭 global/workspace 新 admission，已接受 owner 通过 nested capability
完成 terminal settlement，phase two 再限时 join 并把失败写入 lifecycle receipt。reservation 与
admission seal 在同一锁内线性化；CommandCell 在 framework prepare 前、RunDriver 在 owner 发布前
预留 operation，因此 join 返回后不能出现 late accepted operation。phase one 还启动 framework
command manager shutdown，先取消长命令再 join；terminal repair 有上限，耗尽形成 typed
projection debt，observer join 有总 deadline。active operation 同时阻止 workspace eviction。
Workspace 通过 idle proof 后先提交 Closing，再执行不可逆 shutdown；host 原子 claim 并启动唯一
state-owned shared settlement。eviction caller drop 不会取消 owner 或丢失已消费的 debt。
cleanup degraded 的 generation 留在 registry 中保持 sealed，后续 control/open 与二次 eviction
不能重新开放它或吞掉原始 debt。framework command shutdown 是可重复 await 的 shared result，
并发 caller 不会覆盖 owner。

ChatEventLog 的 async safe point 通过 bounded product-data adapter，并让 blocking closure 捕获
exact workspace I/O receipt。manual compression、GUI cancel 和 HITL orphan projection 在 caller
drop 后仍持有 generation；workspace delete 不能先删文件再被迟到 append 复活。

排队 resume 的 identity 冻结 continuation route。已退役console的旧适配不再参与新执行；保留surface不读取 run snapshot 或严格
预判 journal sequence；真正判定统一由 store 原子 resume transaction 完成。identity 后只有
`execution_path` diagnostic Note 的 suffix 可以继续，其它状态、附件或 continuation 变化仍拒绝。

chat preparation/projection、background submit/control/query、boot reconciler、revision runtime、
Subagent control、GUI/channel TaskRuntime surface 和 unattended worktree control 都复用
该边界。已接受的复合文件 mutation 把 driver registration 随 closure 一起持有，因此 caller
drop 不会提前释放 workspace generation。

Tauri mutation 使用 ts-rs 生成的 `TaskRunControlReceipt`、`TaskRunResumeReceipt` 和
`TaskRetryReceipt`。Continuation resume 的 `turn_id` 是正式 wire 字段；surface capability
来自已注册工具与 workspace facts，不再由前端传递 mode DTO。

## Subagent execution 投影

bootstrap、pooled conversation 和 workspace Agent 共用一个 framework
`SubagentEventBus`。app-core 只消费它的 versioned envelope stream。formal event 通过
TaskRuntime owner registry 定位；没有 run 的事件通过 exact active foreground turn 定位；
当前 workspace focus 与 execution-id 字符串格式都不是身份输入。

进程级 Subagent admission 在 Agent 构造期或 bootstrap 后 task tool 注册期安装。TaskRuntime
不会在 `task_execute` 内再次获取同一 Agent 的 write lease，因为外层 ReAct turn 可能正持有它。

app-core projector 把 framework metadata 保留在 generated `ExecEvent`，向既有
`ChatEventLog` 追加事件，并调用既有 tool detail projector。sequence 跳跃先走 framework
replay；无法恢复的 transient suffix 形成 `subagent_stream_gap`，保留的 terminal 仍用于
对账最终结果。恢复会扫描 retained、active 与已知 stream 的并集；active publisher 的
dispatch-start anchor 可在完整 replay 被挤出后继续提供准确地址。tool detail 失败从已提交事件
重试，并可由 ChatEventLog 重建。active GUI/channel sink 通过 journal 的 weak live
registry 接收已提交事件；GUI与IM后台事件由 committed projection stream 及其有界
late-subscriber/lag replay 投递；请求级 channel 在下一次响应中从 durable conversation cursor
重放 execution event。因此 live 与 replay 使用同一个 `ChatDriverEvent::Execution` payload。

`execution_path` 只作为原 journal 中的内部诊断保留，不生成普通聊天气泡；不通过删除用户文本清理诊断。

Tauri不订阅raw Subagent event；Tauri 只发布已提交的 chat envelope 与 tool
summary。详见 [ADR 0040](../adr/0040-app-core-subagent-event-projection.md)。

## 取舍与影响

- EKO adapter 不再拥有 executor、validator、retry loop 或 descendant traversal。
- EKO adapter 不再拥有 raw Agent stream loop；普通 background 与 boot 恢复后的 PlanTask 执行
  通过同一 `AgentTurnDriver` 路径，并共享相同的 terminal/usage/tool 投影约束。
- Direct completion 保留已观察到的只读 evidence、verification 和 artifact；成功的直接文件
  写入仍不满足 fixed direct-summary contract，必须走正式 writer PlanTask。
- Independent planning Agent 在 invocation 前把完整 registry（含动态 plugin/MCP）的
  Write/Execute tool 加入 `disabled_tools`，producer handler 因此不会执行；sink 同时按 canonical
  tool identity fail closed。动态 Read tool 保持可用。
- 当 primary RunTurn 仍 active 时，Task graph quiescence 不单独写 Goal Completed。Goal
  `RunStatusChanged(Completed)` 与 `RunTurnFinished` 在同一 journal batch 提交；direct summary
  graph 也先保持 Running，交由同一 terminal batch 完成，因此不存在 Completed + active-turn
  crash window。
- framework trace 没有 Paused 变体，因此 EKO 不写该可选诊断记录，绝不把 Paused 伪装成
  Completed。
- Review、summary 和 touched-files extension 只有 exact claim CAS 成功后才发布；stale claim
  的候选结果直接丢弃。
- Subagent durable recovery 使用 revision、attempt、physical execution id 和 TaskStarted
  线性化边界；合法的 restart handoff 可复用，迟到旧 release 不可越过新 claim。
- Async surface 不再直接执行 TaskRuntime file I/O；source inventory 固定所有 production
  surface、boot、Subagent、projector 与 lifecycle/workspace owner 都必须经过同一 adapter 和
  store operation supervisor。
- Projection refresh 已返回 committed/degraded typed outcome；Todo、latest Summary 与 Completion
  Gate 使用 checkpoint-backed projection，Artifact/Review 历史使用增量 segment。10k/100k
  release 规模验证仍属于 Final Integration Gate，不能仅凭实现存在宣称已通过。

## AgentPool 与默认 Subagent 的记忆装配

同一应用 generation 已选定的 `Store` 是父 Agent、默认继承实例和默认 Fork 工厂的共同
记忆 owner。全局池从已启动的主 Agent 提取原 Store；工作区先选定匹配的 Review generation，
再将该 generation 的 `memory_store()` 交给原池。工作区地址、generation 和原运行收据仍由
各自 owner 管理，相同 FileStore 文件路径不构成相同 Arc 身份。

池在调用原 `create_agent_with_diagnostics_and_event_bus` 时传入所选 Store，使它先进入
原 builder，再注册默认 Subagent 和捕获 Fork 闭包。之后从已构建父 Agent 提取的 Store
同时用于继承实例与每次真实 Fork。不能在工厂返回后只替换父 Agent 的 Store 注入通道，
因为已经构建的子实例和闭包仍持有原 Store。独立 factory 调用没有传入共享 Store 时，
继续沿原路径创建本身的 Store，并使用原 fallback；不额外打开第二个共享 Store。

同一个 Store 不代表共享可变对话上下文或执行实例，也不要求为每个默认 Subagent 安装
同一个完整 layer manager。只读工具范围、TaskRuntime 的 Run／depth／lineage、工作区与
Fork 隔离继续沿用原限制；本修复不重新注册工具来绕过 Store owner 检查。装配验证需同时
检查最终父 Agent、继承实例和实际 Fork 工厂的 Store Arc，以及 sentinel 在所选 Store 中的
读取／写回和跨工作区不串写；创建级检查不替代原 TaskRuntime/Fork 执行和退出结算验证。

## Background Review 生命周期

GUI与IM把框架惰性 handle 交给 app-core 的 generation-bound owner。owner 在首次 poll 前
持久准入，持有 generation 直到证据结算，shutdown 时取消并 await 已准入任务；启动时在发布
主 Agent 前恢复未完成收据。详见 [ADR 0043](../adr/0043-background-review-owner.md)。

## 业界依据

- [Temporal Workflow execution](https://docs.temporal.io/workflow-execution)以 event history
  恢复 durable execution，并把重试策略与业务结果分开。
- [Temporal Retry Policies](https://docs.temporal.io/encyclopedia/retry-policies)把 retry 作为
  明确策略，而不是由任意状态写入隐式推断。
- [LangGraph Persistence](https://docs.langchain.com/oss/python/langgraph/persistence)使用
  checkpoint/thread identity 恢复 graph。
- [LangGraph Interrupts](https://docs.langchain.com/oss/python/langgraph/interrupts)在 durable
  checkpoint 上恢复，而不是把暂停当失败并增加 retry。

这些实现共同支持本项目的取舍：持久事实与可重建投影分离、恢复必须绑定稳定 identity、
暂停和 retry 是不同语义、产品 policy 留在 adapter 边界。

## 不适用范围

`echo-agent-learning` 用于验证 framework public facade，不应引入 EKO 文件 journal、review
或 worktree policy。`echo-website` 是静态展示站，不依赖 agent runtime。因此本决策不修改
framework examples 或 website。

后台 `cancel_run` 工具使用 `cancellation_requested` 返回原 Store 的取消请求接受布尔值。活跃运行路径只持久化 interruption 并通知原取消 token，Run 仍可保持 Running，执行 claim 继续由原 owner 持有。最终取消状态和物理结算须观察原 TaskRuntime 与结算收据；工具答复不发布 `cancelled` 完成镜像。工具名、`run_id` 参数、Store API 与取消/结算逻辑均未改变。

## 发布失败与结算证据

插件 bootstrap 失败后，清理义务仍持有原 Agent、框架 wiring receipt、名称 registry 和
其它原资源 owner。下一次 bootstrap 仅按项目目录检索债务，在发布新代前使用原 owner
重试；未结算则构造失败。取消 shutdown 等待者不会取消独立持有资源的关停操作。
参见 [ADR 0022](../adr/0022-framework-prepared-plugin-generations.md)。

插件变更的提交事实与组件结算分开呈现。既有 `plugins/mutation` 成功回执只在原
authority 完成变更后产生；其 `degraded` 仍保留 `success=false`、诊断和目标结算结果。
设置页据该 typed 回执刷新真实目录与配置，并明确提示“已提交，部分组件未启用或存在诊断”。
真正失败、已回滚或仅验证的回执不触发提交后的刷新。迟到结果仍受原 view、workspace
generation 和列表请求序列守卫约束，不得污染新页面；没有新增存储、状态机或公开回执字段。

MCP 后台操作与配置 generation 变更共享准入边界。已经失效的旧操作不能在新代确认
连接后再启动 preparation；已经开始的 preparation 则由原任务持有直到清理完成。

TaskRuntime 按框架最终结算决定是否接受 attempt evidence。`Superseded` 对所有请求
类型拒绝旧证据，不追加旧产品结果，也不为该 attempt 发布终态事件或唤醒 continuation。
读到当前任务状态不代表旧 claim 获得发布权；健康 blocked/failed 的诊断证据仍保留。

## Canonical 重放与 Review 展示

任务 journal 中存在 canonical `TaskExecution` 时，它是完整 execution 记录。旧 flat status、claim、result 和 retry 字段仅在该记录缺失时作为 fallback；记录损坏或 task 身份不匹配必须拒绝重放，不能借 fallback 绕过。

Review 候选、处理错误、自动 Memory 持久化和应用 Review Inbox 收据是独立事实。保留GUI/IM视图继续呈现候选和错误，区分已持久化、未持久化和状态未知；只有 owner 返回真实 evidence receipt 时才能显示对应 Inbox 条目。展示层不产生新的 Review 结果。

EKO 注册的 revision store 通过同一个 run 文件锁和原子 journal 批次提供 execution CAS。claim、中断和 execution 变化由框架计算、校验；应用适配器比较完整 expected execution map 与关系 revision，拒绝关系、context、spec 漂移，execution-only 变化不产生新的 plan revision。abandon attempt 的 journal 与直播使用同一最终提交状态和摘要，被覆盖的请求失败原因仅保留为带精确 claim 身份的独立诊断。


## 产品收敛处置清单

本节随独立终端源码删除包采用，描述当前源码owner；构建、锁和原生验收另列。
退役前完整基线 `c262cfed8e77abed38a27c1fff93547997ec9486` 仍由本机固定
`pre-retirement-2026-10-24` tag与完整能力分支保留。旧 `src/cli` 所有权只属该历史
截点，不能与本表当前GUI/app-core路径混用。见[ADR0050](../adr/0050-gui-product-convergence.md)。

### 终端入口与保留owner

| 当前源码与真实消费 | 移除面、保留合同与验证边界 |
| --- | --- |
| 既有`src-tauri/src/main.rs`→`src/tauri/desktop.rs::run_desktop_entry` | 旧main／cli／tui／JSONL／standalone channels分派和专属provider/config/deps退出；默认gui仍选择echo-agent-tauri，不新建launcher。 |
| 独立`DesktopArgs`／`parse_desktop_args` | 只保model（-m／MODEL_NAME）、project、config、MCP override，保Finder过滤和原优先级；旧console resume/permission/rendering参数拒绝，不作桌面模式。 |
| `ApplicationServices::compose`→app-core `ChannelRuntime`／`management`／`input_pump`／`outbound` | desktop安装GUI HITL factory后实际`start_configured`；原sender身份、incarnation、input-pump关闭/join、回复/工具投影及channel HITL保留，不再由src/cli拥有。真实手机认证/收发与source/focused分别验收。 |
| app-core `auto_memory::AUTO_MEMORY_ENABLED`→GUI panels | 保同一进程全局Atomic、默认true与Relaxed语义；GUI先核原workspace/generation/evidence lease，console adapter不再消费，不新增store。 |
| app-core `ApplicationLifecycleOwner`／AppState／scheduler | 保原File scheduler定义/occurrence ledger、TaskRun与pending conversation/Agent delivery启动恢复。Tauri IPC/面板仍是薄适配；不另建GUI scheduler或自动重放未知effect。 |
| GUI `terminal::create/write/resize/close/list`→app-core `TerminalService`／project_ssh | 保PTY/xterm、系统SSH、Agent shell和原物理cleanup owner。独立终端产品删除不扩删这些；真实远端/native验收不从源码推出。 |
| app-core HITL dispatcher／pairing／channel_provider与Tauri handler | 保GUI/IM竞争身份、取消、Task/Subagent、附件/记忆和共享恢复。专属Repl/Tui provider删除；适用start失败断言迁真实GUI ChannelRuntime receipt，manager start/stop失败分别保断言。 |

CloseRequested沿原prevent_close+hide保进程后台；ExitRequested只begin原owner一次，
run_return后services.join／settle.wait_final结束再离开Tokio。重启保持原身份、occurrence
幂等与恢复，不因窗口关闭、消息phase或普通工具结果补成功，不静默重放未知副作用。
保留路径仍跑原适用回归与native验收；源码准备/采用不冒这些通过，也不额外设全SSH/微信
人工矩阵完成才可准备独立leaf的门禁。

### 退役前LSP消费者历史对照

以下记录原LSP消费者解除依赖时的处置范围，属于历史路径，不表示当前仍有这些注册、
controller或console入口；当前插件旧声明沿保留的generic diagnostics和原prepare generation处理。


下表是固定完整能力基线的退役前源码清单。EKO 源码现已移除这些运行时消费者；保留该清单
用于恢复和逐项对账，不宣告当前仍支持 LSP。框架物理删除和 SDK 验收各自结算；本次消费者
改动的 Rust/GUI 验证随交付记录归档。

| 消费者 | 精确的后续去依赖范围 |
| --- | --- |
| feature 与工具注册 | 在实现结果中移除根和 app-core `Cargo.toml` 对 framework `lsp` 的请求。`runtime.rs::register_lsp_tools` 在 bootstrap 和 `agent_pool/pool.rs` 的 workspace Agent 创建时调用，注册 diagnostics、definition、references、hover、status 工具。移除两条调用路径和 manager owner，不能只删 GUI。 |
| 插件准备/发布 | `plugin_components.rs` 消费 plugin LSP document 并验证配置；`plugin_runtime/{types,runtime,publication}.rs` 拥有 `PluginLspRuntime`、配置发现、代替换、reload、shutdown、`LspServer` capability 和语言计数投影。仅移除 LSP 字段/分支和消费者，保留 prepared plugin receipt、rollback/debt owner、Skills、Hooks、MCP、Subagent、monitor、theme、output style。 |
| 配置与共享命令适配 | 从 `config_discovery.rs` / `config_watcher.rs` 移除 `.lsp.yaml` 发现/监听和代重载分支；从 `extension_commands.rs` 移除 LSP command/kind/receipt，从 `extension_control/service.rs` 移除 LSP dispatch，从 `developer_commands.rs` 移除相关帮助。核对 `src/cli/cmd_impls/developer.rs`、`src/cli/extension_surface.rs`、`src/cli/channels.rs` 和 `src/tui/{commands,events}.rs`，保留其余 extension dispatcher 与 watcher。 |
| Tauri 设置与工具调用 | 移除 `src/tauri/commands/lsp.rs::lsp_control`、`commands/mod.rs` / `src/tauri/mod.rs` 中的模块及 invoke 注册；退役 `LspPanel`、`SettingsDialog.tsx` LSP 导航、`uiStore.ts`、`api/endpoints.ts` 和相关测试。`commands/config.rs` 是保留的通用配置 API，并非独立 LSP controller；核对受影响投影，不整包删除。 |
| 生成与说明消费者 | 只在实现阶段用既有生成器收窄 `web-frontend/src/generated/` 中受影响的 extension/LSP 类型和 barrel export，并同步 docs、examples 与 feature/GUI/endpoint 检查。本次文档结果不改生成物、依赖锁、CI 或示例。 |

旧 `.lsp.yaml` 配置及插件请求退役的 A2A/LSP 能力时必须明确诊断已退役/不再支持。
移除解析或发现不能静默忽略这些输入，也不能假报组件加载成功。仅保留必要的输入识别与
诊断边界，不保留已退役的执行器/控制器。验收须覆盖旧配置和插件请求，以及保留插件组件
的真实加载结果。用户会话、任务、记忆、插件文件和配置文件不得作为过时代码一并删除。

在当前基线的根/app-core manifest 与 `src`/app-core Rust 源码中，搜索未发现 EKO 直接
请求 `a2a` feature 或 `a2a`/`A2A` 生产调用。这个有界否定结论不等于框架/SDK 已盘点。
它们的 owner 负责实际公开 API 与精确 accepted 合同退役；普通 AgentRouter 消息、Subagent、
MCP、IM 继续保留。

### 顺序、保留与证据

1. 旧 framework API 仍存在时先让 EKO/SDK 解除依赖。EKO CI 跟踪 framework main，SDK 有
   独立精确 framework revision，不由顶层 gitlink 替代。受支持消费者已能无 A2A/LSP 构建后
   才删除框架实现及宣传；新增 framework API 继续先交付框架、后消费者采用。
2. 先验收 GUI 所有的 IM/cron 启动、配置、失败/重连、HITL、共享恢复、隐藏窗口运行及真正
   退出清理，再退役终端入口。保留 ConversationStore 事实、TaskRuntime `events.jsonl`、
   可重建投影和文件持久化，不引入 SQLite 或第二套 graph/store。
3. 保留完整能力 branch/tag 作为恢复锚点；恢复时新建任务分支并按届时 main 重评，不改指保留
   refs。Exa `4343f0b7` 仍是本机独立提交，不隐含推送/合 main 授权。框架/SDK/官网清单与
   跨仓恢复证据由各自仓库/集成负责人拥有。
4. 本切片验证双语文档、源码引用和 diff 范围；真实运行、原生桌面、IM 投递、cron 恢复与
   最终消费者矩阵由后续实现结果验收。历史 ADR 中的 surface 对等和 LSP 实现说明记录旧行为，
   不覆盖 ADR 0050 的未来产品范围。

## 本地与 SSH 交互终端

`TerminalService` 持有原 PTY、child、reader thread 和保留的 settlement handle。
桌面表单接受 host 或现有 SSH alias，以及可选账号、端口；系统 `ssh` 接收独立 argv，
处理认证、既有配置与 host-key。EKO 不保存凭据，不把本地进程启动解释为登录成功。
用户手工操作与 Agent ShellTool 权限路径分离。

各 tab 先准备 xterm 与 Tauri listeners 再 create，切换或隐藏仍保持挂载。raw bytes
直接交给 xterm；scrollback 限 2,000 行，输出队列限 1 MiB，输入队列限 64 KiB，
两者均限 256 个等待块。lag/超限明确可见，不支持历史重放、自动重连或命令重发。
输入超限会丢弃未发送队列并停止该视图后续输入。

关闭仅限制观察等待，原生操作继续由原 owner 结算。只有原 child wait 和 reader join
均完成才移除会话并发布一次带真实 code/signal 的退出凭据。kill/wait/read 错误保留
同一 owner 和目标；不可恢复的 read 错误不能靠重试抹除。应用退出先封闭准入再排空，
失败时保留既有 lifecycle owner。direct-child 结算不证明整个进程组或远端副作用停止。
见 [ADR 0051](../adr/0051-ssh-terminal-sessions.md)。

验收边界：本变更当前只有源码与 Node 证据。Rust 测试、新 Tauri 二进制、原生窗口/主题
及真实远端认证交互均未完成资格验证，不宣称平台支持或发布完成。


## 绑定的子智能体政策

EKO 配置只提供新运行默认值。原 RunCreated 的执行 profile 拥有该 Run 捕获的层数，journal、checkpoint 和 resume identity 保留它。primary 为 0，真实 child 为 1；原 TaskRuntime 与框架 dispatch 继续执行，不新增任务图或政策 store。见 [ADR 0052](../adr/0052-gui-subagent-depth-policy.md)。源码资格不替代原生或精确 main 验证。

### GUI 渠道、cron 与真正退出

原 IM owner 归位 app-core `channel_runtime`，bin 入口仅转发；ApplicationServices 与 Tauri 共用同一 Arc。ConfigState 只在 owned 持久化成功后发布 UUID revision，transport epoch 与 applied revision 保持各自含义。原生命周期 channels slot 持有 manager/coordinator/pump 句柄到真实物理结算；所有权未知时阻止依赖提前关闭。桌面 X 隐藏；明确退出在 Tokio 存活期间等待原 receipt 的 final 发布，其它 bounded 观察不变。已结算失败可为 final Err，Unknown 不构成 final。Cron 视图只消费原 edit/manual/history 收据和已有精确 run 事实。数据流、恢复边界、参考实现及待验 Rust/native 范围见 [ADR0053](../adr/0053-im-cron-gui-control.md)。

Scheduler 的桌面创建适配遵守 Tauri 默认顶层 camelCase 参数：Rust 的 `cron_expr`
通过 IPC 发送为 `cronExpr`，`apiInvoke` 不自动转换键名。表单、REST JSON 和编辑命令内
`expected`／`edit` 的 serde 字段仍使用 `cron_expr`，不能递归改名。运行历史游标仍为
原生产端生成的 opaque JSON 字符串或 null，前端原样传递，避免解析 u64 后丢失精度。

## 绑定到 TaskRun 的领域插件

专业方法通过原插件 prepare/publication 读取 EKO `domain.yaml`，不再由四套编译内
profile 模板选择。原 RunCreated 事件保存唯一完整不可变绑定；TaskRun、graph metadata
及 task extension 携带同一有界身份引用，原 journal/checkpoint fold 重建完整规则。
动态图修改保留绑定、工具上限及最低 artifact/check/语义义务。

primary、计划 Subagent、Verification 与 review 消费持久规则。原 pool lease 携带
发布代和实际 Agent catalog 句柄，跨 workspace 目标也核自己的同一代。恢复执行核
原身份，不用当前包静默改写旧任务。普通 General 对话保留原无 plan 快捷完成；轻量
领域使用原固定 Summary 事务与真实事件证据，有正式义务则走真实图和原 CompletionGate。

当前仍为源码实现：adhoc `agent_tool` 沿原物理 lease 投影与同一 registration 准入，
原 model projector 在无结构化 prompt payload 时读取持久 method；GUI prepare 复用
原 foreground 令牌。这些路径已新增回归定义，Rust 类型/测试、TypeScript 原生成、
完整集成及 native/main 验收待做。见 [ADR0054](../adr/0054-task-bound-domain-plugins.md) 和
[包示例](../../../examples/domain-plugin/README.md)。

## GUI 正式任务历史读取

任务上下文面板发现所选 Workspace 内的原 TaskRun。库存与精确选择扩展既有 taskRuntimeStore；普通 GUI 会话仍由 ConversationStore 拥有。渠道入口的 `channel:` 会话与调度 occurrence 的 `cron:` 会话，无论成功、失败、取消或是否生成计划，都进入同一个历史可见性判断。普通 GUI 轻量回合仍沿原 provenance 规则，打开历史不会创建计划或把普通回合升级成任务。

侧栏最近任务复用 `list_task_runs`，按 `(workspace_id, product_data_generation, run_id)` 去重，同一 IM 会话的不同运行分别保留；已有运行行时不再重复显示对应会话行。来源来自应用入口拥有的会话命名空间，状态直接读取原 Run，不用来源、日志文字或 `attended_mode` 猜测失败/取消。`list_task_runs` 将 generation 和筛选条件绑定到每页最多 20 条的实时页，首屏每个 scope 读取 8 条，更多页只在用户要求后续读。读取失败保留已显示内容并反馈错误。

页面可见时每五秒刷新已请求范围内的有限 Run 页，至多两个 scope 顺序读取；隐藏时停止后继轮询，回到前台立即刷新。周期刷新不扫描完整会话列表，初次显示、工作空间/GUI 会话变化和回前台才沿既有会话元数据入口更新。关闭页面会注销监听并取消后继刷新；迟到响应必须仍匹配原 scope/generation。失败早于 transcript 保存时仍可直接打开原 Run 详情，不补造会话、计划或第二套历史 store。

选择旧 run 后，详情、轮询和控制使用其精确 run_id，而非会话 latest。运行页/事件页不建立第二个任务或事件 store。事件读取返回有限片段、无精度损失的后续 sequence 和未读尾部标记，Subagent 历史未读完仍明确提示。原类型化控制回执区分 no-op、已接受请求、排期/恢复重试与物理或语义完成。同 ID 工作区换 generation 以及迟到请求由原 scope 和 GUI 选择世代隔离。

有限传输不意味着已有索引或常量扫描。合法任务图/查询测量，以及浅深色、常用/最小窗口的原生行为仍需验收。Framework/SDK 示例不变，官网等待合格 EKO 源统一镜像。参见[任务面板指南](../guides/gui-task-runtime-panel.md)与[ADR 0008](../adr/0008-taskruntime-bounded-query-projections.md)。

历史检查在同一个 Zustand store 中至多额外保留一份呈现快照，不替换当前聊天的 activeRun、任务数量、失败提示或消息归属。任务面板操作携带实际显示的 run_id；冲突提示的 Continue 使用原准入 Workspace/run 与类型化回执，不读取选中历史的状态来判断目标。迟到结果不能清除后继提示或覆盖另一个呈现快照。

## 会话事件投影与终态

同一任务的续接保持原 `root_message_id`，当前有限回合仍使用自己的 `turn_id`。原生事件的 message 身份必须指向原根消息，不能把续接 turn 当成新 journal 根；原 journal 的身份校验继续严格执行。IM 的 execution/input 生命周期事实完整保留在原 journal，手机不逐条接收这些内部 ID 和调试状态。真实失败与取消只由原 typed Terminal 生成用户可读提示，不把断流、保存回执或模型片段冒充最终结果，也不重写已保存的旧终态。

前台等待续接后，首个有限回合的 receipt、原生执行状态和 `RunTurnFinished` 保持原值。只有原受控 completion 明确为 `Stopped + Completed`，并且原 generation 下读回的同一 Run、工作空间、会话、TaskRun 根消息均匹配且 Run 已 `Completed`，才把已结束前台的聚合呈现为完成。该只读观察沿原 foreground slot 携回，不持久化、不参与准入或执行，也不替代 TaskRun 权威。`Deferred`、`Paused`、取消、身份漂移、未观察到物理释放或下游投递债务不获得这项成功呈现；迟到观察不能写入后继 slot。

前端接纳框架 AgentEvent schema5 的 `transcript_projection_settlement`，完整保留保存结果分类、操作与会话/世代身份、尝试次数及错误信息。它是会话保存检查点，不是 TaskRun 或聊天回合完成；成功保存不改变运行状态，未确认、阻塞与冲突显示明确提示，真正的回合结束仍以原 `turn_status` 为准。

事件排序器只有在对应投影处理成功后才消费该序号。投影异常保留原事件和游标供后续重试，不能提前删除事件后把终态永久挡在缺失序号之后。恢复中的活跃回合查询也受已观察到的终态版本约束：实时终态抵达后，先前发出的迟到快照不能重新建立旧执行槽。界面修复与受控模型验收脚本分别验证；脚本必须通过原 `tool_search` 激活延迟曝光的 HITL 工具，不能凭普通回复或模拟前端状态认定原生人工确认已通过。


## 2026-10-27 裁决：Steer 由主模型理解，已有 Goal 仅用户可改

用户明确：普通Steer由主模型结合历史理解并在执行中按需更新计划，不先调用独立模型，
也不强制串联“有效要求更新 → 计划调整 → 验收调整”。该裁决替换旧Plan14的独立理解和
强制传播承诺，旧原文修复、接纳、幂等、错误与恢复证据保留各自真实范围，不改称全部通过。

TaskRun.goal是必填字段，未建立该运行目标时复用原初始化设置初始Goal。已有Run的重试、
ensure_scope与create_run重入返回原目标；普通Steer接纳/消费和模型task_update不能改写已有
Goal、goal_revision或goal_hash。用户通过GUI目标编辑或IM `/task-goal` 显式修改，继续使用
原RunGoalActorSource、expected_goal_revision/CAS和安全点；用户目标变更的原版本失效规则保留。

用户“结束并删除当前目标”沿原结束/取消入口退出活动目标呈现，不向后端写空Goal或抹去
历史事实。普通执行取消同样按原生命周期结算并保留goal/revision/hash，不能把取消运行等同
修改或历史删除Goal而另加权限门控。原Stop、HITL、恢复与物理结算owner不变。

必要历史、完整长原文及末尾限制继续沿原journal、压缩投影与read_artifact读取；缺失或
校验失败时原required_context_unavailable暂停相关新副作用，原Goal和可恢复owner保留。
这里不新增GoalStore、要求解释器、第二份队列/状态机或结构化模型协议，也不裁剪通用框架/SDK API。

### 原始输入历史来源族

TaskRuntime的`events.jsonl`仍拥有原输入及其稳定身份；`HistoryProjection`扩展的原始输入来源族与既有artifact/review族同属可丢弃读模型。来源族分别保留接纳和投递事实，不进行语义去重、分类、Goal改写或自动计划/验收更新。主模型仍自行结合历史理解普通Steer。

RunAuthority在原产品数据lease下捕获journal identity和固定观察序号，以有界页扫描完整前缀；`run_id`或相同head不足以替代journal代际。原来源族和不可变文件使用来源内容身份稳定发布，现有`InputResourceRef`给恢复capsule提供一个可发现入口，模型继续用`read_artifact`分页取得早期原文。普通工具事件不改原文源字节，新增输入只发布新引用；旧快照/旧cursor不能跨Run、工作空间或来源代际混用。

最终请求资格只接受该模型请求已经收到的准确前缀，并核本次真实工具上界和本机artifact root/SHA。缓存缺失或损坏从唯一journal恢复；源不可读、版本不明、SHA或reader资格失败沿原`required_context_unavailable`暂停并保留owner、Goal和expected-resume，不静默使用空历史或扩大工具权限。所有原文路径取TaskRuntime本机产品data-root，不随SSH执行cwd重定向。

长canonical Goal不借原文来源族伪造输入。原GoalContract/物化接缝把超过既有spill边界的完整当前目标发布到同host artifacts根，引用绑定原Goal revision/hash、Run与journal代际；原tasks/objective继续归档。旧Goal引用保持文件不变，当前资格只接受用户最新权威版本，legacy没有Accepted的初始化目标同样可读。短Goal与已完整内联的原文不增加reader要求；实际被省略的必要全文才必须具有本轮真实read_artifact资格，读取失败仍按原Pause/expected-resume处理。

完整可见性的判断以最终实际模型请求为准：既有 GoalContract 能内联的边界与未启用 continuation 时的恢复预览边界分别沿用；预览会省略的 canonical Goal 发布 host 引用。当前 PreparedUserTurn 已完整内联的中段文字（超过预览长度但未到 spill 边界）保持 NoTools／read_file-only 正常执行。此前真实配对 read_artifact 调用和 Tool 结果若仍完整留在请求中，按分页字节范围、全文覆盖及原 SHA 核对后也算全文可见；缺页、重叠不一致或 SHA 不符不算。核对不会从本地文件补入缺失模型文字，也不把“文件存在”当作“模型已读取”。同一 Goal 的已发布引用资格仍绑定 Run、journal、revision 与 hash，不随当前投影显示模式改变。

## 独立终端退出后的当前owner

本节随EKO终端删除包采用。DesktopArgs四字段与数据根不依赖旧console；desktop经原
AgentRuntime::bootstrap／ApplicationServices::compose接IM、cron、Task/Subagent、HITL、
记忆、附件、会话/任务恢复和退出owner。关闭窗口仅hide；真正Exit停止准入，原services
join／settle.wait_final返回后才离开Tokio。GUI terminal／SSH、Agent shell与SDK stdio保留。

旧src/cli路径对照及tui/cli来源枚举只说明历史。当前没有独立REPL/JSONL产品或新Task CRUD；
原运行时结算不等于Goal完成，UI、文档或旧focused结果不补造删后构建/锁/native/正式pin资格。

### IM 服务停止后的原投影排空

普通 turn 取消只结束原 turn，继续保留 IM service；真正关闭 service 才取消其原 durable token。已取消的 deferred sender 继续消费原 renderer 与有限事件队列以取得真实 EOF，停止实际发送后不再为这些被抑制的输出等待发送节流。活跃 service 仍按原 burst/interval、脱敏、UTF-8 分块和消息/字节预算发送。该 token 仅沿既有 EKO IM 投影适配传递，不新增执行 owner、队列、通用取消状态机或伪造完成；错误、原 driver 与 JoinSet 仍须实际结算。此内部关闭修复不改变 examples／官网公开 API，不代替真实手机或原生 GUI 验收。
