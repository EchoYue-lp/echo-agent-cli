# ADR 0057：原生消息消费与 Eclip 进度报告策略

## 状态

2026-10-27 用户已确认方向。本仓正在实现；源码、定向验证、Framework 合同和新版原生验收分别记录。本 ADR 不表示跨仓切换或产品验收已经完成。

## 背景

旧聊天消费者根据 `ThinkStart → Token → ThinkEnd` 推断正文属于思考。这会把正常操作前说明和进度写进 Reasoning，也不能表达同一响应的多条 assistant 消息。工具输出、状态和自然语言报告的来源不同，不能彼此替代。长工具或长模型请求期间，用户还需要知道实际等待的操作和时长。

## 候选方案与决策

选择消费 Framework 已归一化的原生消息事实，并沿原 ReAct 执行结算。正文、Reasoning、工具和运行状态由显式类型区分，保留块身份、模型请求、阶段、顺序和增量事实。缺失阶段保持未知，不根据中文措辞、是否夹带工具或旧起止信号猜测。最终回答候选及 Guard、Critic、Stop Hook、持久化和结算归 Framework；EKO 消费确认后的发布和终态，不新增完成判定器。

EKO 的报告策略放在既有 `EkoContextProjector`，使用原模型安全点。报告规则要求首次工具操作前及关键发现、方案变化、阻塞或验证结果变化时简短说明。距离最后一次公开自然语言报告达到 60 秒后，只在下一次原模型请求安全点提醒一次；报告后重新计时。同一 iteration 因压缩而重复投影仍保留同一替换式提醒，后继 iteration 不累加提醒消息。

报告计时只属于当前执行的内存观察，不持久化为 Goal 或要求状态。使用单调时钟；观察者保留原注册 UUID，迟到旧执行不能重置同名新注册。只统计真正向原 sink 发布的 assistant 自然语言块和确认最终回答，Reasoning、最终候选、旧 Token、工具日志与状态不计作报告。Rust、全链路合同及原生仍需分别验证。

GUI 显示实际模型或工具事件所指明的操作、该操作耗时及最后实际事件时间。观察绑定原 workspace、conversation、root turn 和 turn；重放使用原 envelope 时间，无关输入 frontier 只刷新其所属队列。模型请求使用实际事件或请求身份单独计时。等待期间 UI 定时器只重绘耗时，不推进最后事件、不写助手正文、不产生“新发现”或完成。当前执行结算及切换历史时清除此瞬时状态。

未采用额外计时模型循环：它会与长工具竞争并改变原执行时序。未采用工具日志充当进度：日志只证明工具活动。未采用前端倒计时强制结束：等待时间不能证明执行结算。

## 影响与保留合同

- 普通 Steer 仍由主模型结合历史理解；报告投影不改已有 Goal、任务计划、验收或 durable 输入。
- 同执行的 Stop Hook 继续仍是同一执行；Agent 执行结算不代替整个 TaskRun 目标完成。
- GUI、IM、cron 与 Subagent 各自呈现同一事实，不获得另一套结束权威。
- 整段 Guard 模式下，未通过的最终内容不能提前公开；显示层不能通过流式缓存绕过。
- 旧日志保原件，只读归一历史；无法判断的旧控制文字显示为历史执行说明，不重新注册旧工具或补成功。

## 验证与文档同步

前端回归覆盖旧思考信号不改变 Token 正文类型，以及长等待只推进耗时、真实事件推进最后时间、完成和历史切换清除状态。Rust 回归覆盖 59/60 秒边界、重复投影、后继请求不重复提醒、报告重新计时和旧 observer fence。实际 Frame 块接缝、整段校验、唯一终态、历史恢复、IM/cron、完整门禁和新版 Tauri 必须各有新证据。

本次未新增公开模型工具或 SDK API；现有 examples 不需添加 EKO 报告控制接口。相关示例仍随所属冻结快照编译，不运行长 soak。官网由官网正式 owner 在准确源码稳定后同步；未部署不称已发布。

## 2026-10-28：历史 publication 的确认边界

GUI 会话恢复沿已有 `ChatEventLog.replay` 读取原 workspace/conversation 流，再把
`StoredMessage` 转为 `SavedMessage`。raw `assistant_blocks` 只保留 Framework 原件回放；
`approved_assistant_blocks` 必须和相同 `execution_id`、`request_id`、事件 sequence 及完整
typed block 逐字段匹配。`Some(empty)` 明确表示没有可呈现块，不能回退 provider raw。

只有同一 owner 的 `FinalAnswer`、匹配的 `ExecutionStatus::Settled`、完整 UTF-8 正文摘要，
以及产品 `TurnStatus=completed` 同时成立时，`final_answer_confirmed` 才能为 true。产品
TurnStatus 为 failed/cancelled 时保留未确认；Framework Agent 的 Settled 不代表整个 TaskRun
或 Goal 完成。缺少坐标、原事件或来源 provenance 时继续显示未确认的历史说明，Fork 也不能
把目标会话伪造成源 execution owner。

### 未结算状态字段与原生中间块历史

原生中间 assistant 行可以没有 `execution_status`。已经批准且真实发布的 Commentary、
PartialAnswer 和 Reasoning 仍须按原序保留。缺 phase 的普通正文只有在原 canonical
`tool_calls` 非空且批准块逐字段匹配原 publication 时才进入中间视图；其 phase 保持 Unknown，
不按文案推断 Reasoning 或最终回复。不能借最后一条消息的 Settled 回填这些源行，也不能
仅因缺少 status 抹掉原中间块。EKO 只从 canonical approved 块与原 AssistantMessage publication 的逐字段
一致关系派生视图，要求唯一原 execution、stream、workspace、conversation、root、turn、
message、event ID 和发布次序；来源不唯一或顺序冲突时保留原件并报告错误。

Fork 通过现有 `NativeHistoryImport` 在产品 metadata 中原样携带这些进度 envelope。
这类 import 仅包含原 AssistantMessage，不携带 FinalAnswer、ExecutionStatus 或 TurnStatus，
也不改 canonical Message/SHA、源日志、目标 Run/Goal 或 owner。目标冷加载和再次 Fork
复用同一原发布事实，中间块始终 `final_answer_confirmed=false`。普通正文不进入 Reasoning；
明确 Reasoning 使用既有独立折叠呈现。空 delta 不代表没有发布：可读文字只取批准 native
块的公开 delta、Signed.thinking 或 Opaque.summary 字符串列表，Timeline 与外层可见性判断
共用同一提取函数。签名、Redacted.data、Opaque.data/id 不转换成正文，不从 Message 的 raw
`reasoning_content` 或 `reasoning_blocks` 回退显示；原 typed 数据及对应 envelope 仍完整迁移。
同一完整 outer/inner envelope 的重复重放只呈现一次；判断复用已有 canonical JSON 编码，
原身份、摘要、源坐标、序号、时间和载荷均须一致。新的 outer publication 或冲突 inner 记录
仍拒绝歧义；import 验证比较完整原记录集合，不能仅凭块内容相同或记录条数授权迁移。
None、Some(empty)、未批准/未发布的块和无工具无 status 的 Unknown 继续禁止 raw fallback；最终选定回复仍使用上文的
原 FinalAnswer、同 owner 结算与产品 completed 门槛。此处是应用读取/迁移适配，未新增框架
机制、store、执行器或结束权威。


## 2026-10-28：旧控制调用的来源边界与真实getter接线

通用旧历史投影、pending恢复、CAS、候选校验和执行结算继续归Framework。EKO复用已发布的
`PreModelContextProjector::recovered_tool_origins`：`EkoContextProjector`委托其原
`TaskRuntimeContextProjector`，后者及`PreparedProjectContext`消费同一应用来源适配。
这些对象已在原factory、pool／runtime重绑和ProjectAgentAdapter装配路径构造，不新增store、
journal、注册表或第二执行器。getter回传空列表明确表示Unknown，不表示恢复或执行许可。

旧默认工厂确实默认注册FinalAnswerTool、没有默认注册ThinkTool；旧同名add_tool会被拒绝。
但旧公开API也允许`replace_tool`、remove后重新注册和`set_tool_manager`。原checkpoint、
RunStore trace和ToolInvocation没有保存当时工厂／实际注册实现身份。今日factory、当前同名
工具绑定、scope导入代数、匹配call_id／参数、路径或备份时的binary都不能排除旧自定义实现。
当前Custom存在不能追认历史Custom，当前缺席也不能追认历史Builtin。原生ToolCall自带
provider_source时更不能被当作旧控制调用；该字段只证明provider坐标，不证明工具实现。

因此当前EKO适配不签出任何未经原记录可靠证明的Builtin或Custom来源：已配对历史保原件并沿
Frame现有模型读取；未知pending必须沿原restore／明确不可恢复路径拒绝，不能改派今日同名工具、
补假ToolResult、重放已完成副作用或补成功终态。新执行显式调用当前自定义think／final_answer仍走
普通工具pipeline，不按名称退役。已真正可靠绑定的来源可由Frame既有来源接口处理；本轮未找到
可对旧EKO控制call作此证明的耐久正例，不能把默认Unknown接线称为全部旧历史恢复已闭合。

没有要求旧数据持有从未保存的签名。可靠来源缺口若要在未来解决，最小位置是原注册producer在
原事件／恢复记录里保实际实现来源与原call身份，或原import／archive owner提供已有可信记录的
精确绑定；不新增store或第二状态权威，也不能把新字段倒填成旧历史事实。普通无控制调用及已合法
配对历史不因本适配新增门控。实际归档、fixture、定向测试、正式Rust和原生验收分别报告。

## 2026-10-28：普通 source 的耐久 publication 投影与有界读取

### 背景与实际反例

canonical Message 的 Candidate、raw/approved 块不是 EKO 最终发布事实。普通历史先读全部
Message、再用同会话最后4096条replay事件的旧读取，会漏掉仍在盘的早期publication；默认段
裁剪还可能真正删除它们。Fork原已用NativeHistoryImport内嵌原事实，但普通source不应依赖先
发生Fork才获得长期一致性。两条真实复现候选分别隔离tail cap与自然prune，仍须在新源码窗口
执行RED→GREEN，不把源码路径分析写成已通过验收。

### 参考与已有机制

Microsoft Azure Architecture Center 的 Event Sourcing pattern 把不可变事件与只读查询投影分开，
并用snapshot避免每次重放整条历史；本仓Framework journal亦按同一模式提供稳定JournalIdentity、
有界replay_after与FileCheckpointStore。其segmented prune的现有合同明确要求调用者先保存被裁
前缀的耐久投影。EKO复用这些通用能力，Framework不添加产品字段、owner或确认状态机。

参考：<https://learn.microsoft.com/en-us/azure/architecture/patterns/event-sourcing>；本次核对
Framework `echo-state/src/journal/{mod,file,segmented}.rs` 的实际API。该文献只支持事件/投影/
快照分离模式，具体的publication许可与成功门槛仍以EKO已有合同和原事件为准。

### 候选取舍与最终决策

- 扩大replay cap、无界扫描/整日志Vec无法同时保证完整与有界，不采用。
- 永久pin所有native消息会阻止原段retention，不采用。
- 从Candidate、provider phase、approved或summary正文补成功事实会新增终态权威，不采用。
- 在**同一ChatEventLog stream目录与原mutex/生命周期**下，复用FileCheckpointStore保存按原
  publication查询键划分的耐久投影。原StreamJournal仍是写入/sequence/generation权威；投影只
  复制原ChatEventEnvelope，不发新event、不造新execution，不实现另一个Store/owner/executor。

投影复制四类原始wire事实：AssistantMessage块、FinalAnswer、真正runtime terminal，以及同
outer scope最后一个product completed/failed/cancelled。每个execution还保原Agent事件中不同
outer scope的完整record见证，以便原validator继续拒绝跨消息owner；不能把Token/ToolCall都
过滤后抹掉原先存在的拒绝依据。同一完整record幂等；不同事实保两个歧义见证，更多来源明确
unavailable。product状态沿原validator的最大sequence规则，不把早期completed压过后继failed。

### 耐久性、恢复与裁剪顺序

每次safe-point或查询最多读取一批原records。先确认原journal，再逐键原子保存原记录投影，
最后持久coverage `(JournalIdentity, through_sequence, first_observed_sequence)`。只有可靠
coverage从原序号1连续覆盖当前head、且所需checkpoint barrier确认后，才允许既有pin policy
调用原prune。投影失败/未追平时保留原journal，已commit append仍返回原receipt，不盲重试。

冷恢复从同一coverage继续，单次追平有界，未完成返回typed Pending及through/head，不能把
尚未检查的新tail（可能包含失败/跨owner事实）当作不存在。成功查询只加载当前Message的块/
execution/scope所需原记录，不装载整会话日志。查询完成也允许收敛先前deferred retention，
不要求再发一个模型/工具事件，不永久pin原段。

已有被裁prefix若没有可靠原coverage，返回typed Gap；即使后缀命中相同key也不能证明前缀
没有第二来源。foreign JournalIdentity、损坏checkpoint或缺必要scope见证均拒读，后继append
不能将foreign快照当None洗成新证明。已丢且没有锚的旧事实不能靠新投影补造。

### 消费者与产品边界

`native_history_publications(workspace, conversation, canonical_message)`只以Message内容提供
查询键，返回原records。批准呈现、同execution/request/双层sequence、FinalAnswer完整正文/
UTF8摘要、真实Settled和productcompleted仍由原native_progress_publication_events、import
verifier及project_native_history_publication判断；投影不持有success boolean。

普通get_conversation和Fork source读取改为该原子集API，已带NativeHistoryImport的目标继续
走原metadata verifier。MissingPhase、Reasoning或Unknown+tools显示策略由原EKO薄适配维护，
此机制不复制selector。None/Some(empty)/未批准Unknown与最终选定回复门槛不变。

所有派生checkpoint位于原stream目录，原conversation/workspace删除一起清理，复用原stream
lease，不增加后台task/reader。总盘量随必要native历史事实增长，但不复制原Token/工具输出
全日志；查询页/记录批次有界。初次从完整旧prefix追平可能返回Pending；已被无锚裁掉的历史
明确Gap，不谎称兼容恢复。测试、生产以及native App资格分别记录。

完整from-1 coverage已可靠追平后，未存在final/terminal/product key是权威缺席，交原validator保持
未确认；不因当前retainedfloor>1把“从未发布”误报物理gap。已有原同exec事实却缺必要scope见证
仍不能授权成功。新增对应真实prune后Candidate/Final但无Settled/product的读取一致性反控。

### 同一次普通冷载/Fork的自动有界续读（2026-10-28后继）

单步投影的Pending只适合内部进度，不应要求GUI用户重试。正常consumer使用原ChatEventLog的
async读取方法，在同一次请求内部复用既有PROCESS_CHAT_EVENT_IO配额及spawn_blocking，逐批
推进并在批间yield。新publication fold每批最多4096条；原journal cold-open仍执行既有generation/
完整前缀校验与控制pin恢复合同，不能为了赶进度跳过。所有返回只含当前消息所需原facts，
不把全journal装一个Vec，也不创建模型/执行器/后台owner。

读游标只在当前调用栈持有原CachedStreamJournal、JournalIdentity、workspace/conversation和
固定head；不会把目标默默换到重建后的owner。不相关尾部append不扩大该head。若某所需
per-key投影已因并发publication越过读点，则在同一原generation下自动捕获新的有限head并
继续，不能拼接不同head的状态，也不能从新owner偷读。原slot被删除或generation变化严拒。

每个原blocking批真实结束再看取消，不将已在做的派生checkpoint工作丢成“已完成”；取消后
不排下一批。GUI持原workspace control并在返回前核原generation/conversation创建身份；前端
原loadGeneration/workspace/canProject fence继续生效，旧视图结果不能覆盖新视图。Fork沿原
owned source准入/代际scope与同一async读取，不将Pending转成让用户重试的IPC错误。Gap、
foreign/corrupt错误不被续读吞掉，不返回空或确认来伪装完成。

回归覆盖4097/8193条完整旧前缀、无投影且无prune的核心首次单调用、GUI真实consumer首次
载入、原完整owned Fork首用；另核批间取消、固定head不追无关append、原slot删除重建拒绝，
以及async路径原Gap/foreign/corrupt严格失败。新增负载只在fresh fixture清除派生投影模拟旧版
首次采用，原消息/journal records不删、不补terminal。各层测试与原生验收分别取实际回执，
不以本ADR或source review冒真实GREEN。

### 最终正文按原候选身份呈现（2026-10-28后继）

正文相同不能证明两块属于同一最终候选。Commentary、PartialAnswer、Reasoning 和独立
Unknown 进度继续展示；没有可信关联时，保留普通块与已安全收到的最终正文。

既有 `project_native_history_publication` 在原批准发布、Candidate／实际 Settled 三元和
产品 completed 均匹配后，只读回传实际 `execution_status` 与 `final_answer_source`。
后者保存 execution、request、运行时 sequence，以及每个已批准并实际发布候选块的
request、block、模型内 sequence 和 publication event ID。运行时与模型块序号分域；
Guard 排除原候选时集合为空，不从原正文或同文字符串补坐标，也不把 Candidate 改成 Settled。

live 在原产品 completed 后只调用一次既有历史读取，复用冷历史 DTO；按 workspace、
generation、conversation、loadGeneration／epoch 和执行身份拒绝迟到结果。合入只更新
呈现来源，不改变正文、phase、完成状态、TaskRun 或 Goal。renderer 仅折叠与上述实际
终态及逐块坐标完全一致的 Unknown 候选，最终安全正文始终保留。这里复用原校验器和
原 owner，不增加协议、validator、store 或轮询。

完整气泡的 live／cold、单／多块、Guard、不同执行同名块、加载失败与视图切换分别验证。
前端消费夹具只证明呈现；真实后端 DTO 仍须由原 Guard、FileStore、journal／import 链取得
并重放完整气泡，不能把夹具中的 Settled 当作真实后端执行结果。
