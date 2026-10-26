# ADR 0056：个人微信认证、单聊与定时结果投递

## 状态

已采纳（设计）；原 Core epoch 4／Framework F863 的仅 library/channels 检查已通过且零 warning；后续长控制源码尚未编译、无原生验收。
原 pump 的 deferred reply 源码机制已针对 32 槽/严格 cursor/HITL backlog 反例落地，压力验证未执行，完整 Outcome 未验收；长 native 控制已完成源码 owner 接线，仍待真实 consumer 验证。
0056 已由中央作者预留；中央 Domain0054、Project0055 保持其原权威和编号。

## 背景

EKO 是本机个人助理，GUI 是产品主入口，IM/cron 继续使用原 Agent、TaskRun、HITL 和文件权威。
原 ChannelRuntime/ChannelManager 拥有渠道 producer 及 stop/join；ConfigState 拥有配置 CAS 和落盘；原 SchedulerRunner/CronTaskStore/TaskRuntime journal 拥有 occurrence、运行及结果。
账号、手机用户与目标必须明确，GUI 当前项目或新登录不能替换已准入的工作。

第一方协议参考固定为腾讯 [Tencent/openclaw-weixin 874637a0dbc24050f72d1385add4fc6170a2d726](https://github.com/Tencent/openclaw-weixin/tree/874637a0dbc24050f72d1385add4fc6170a2d726)，
2.4.10，原项目 [MIT](https://github.com/Tencent/openclaw-weixin/blob/874637a0dbc24050f72d1385add4fc6170a2d726/LICENSE)。其 QR→本机凭据→authenticated poll→context 回复分层作为参考；OpenClaw host 不是 EKO SDK，固定源码也不是 EKO 自有客户端获准联网或实际收件的证据。

## 候选方案

1. 安装 OpenClaw Gateway 或注入桌面微信：引入另一个 host/所有权边界，不采用。
2. 为微信新建 inbox、scheduler、TaskRun FSM 或 SQLite：复制原权威，不采用。
3. 框架实现通用平台协议及可选 ingress 观察；EKO 持有认证、peer/UI 和通知策略，薄适配原 journal/ledger：采用。

## 决策

原 ChannelRuntime 增加有界认证 slot，保留同一个 challenge producer、取消和真实 join。begin/refresh/cancel/commit 使用原 operation 边界，网络不长期持有该锁；确认提交核对 challenge、revision、实例及 credential generation。
confirmed 必须含完整身份，且原 owned config save/CAS 成功后才能发布授权；already-bound 没有新凭据，不能自行宣称已连接。
数字码、二维码 payload、bot token 和 context 不进入普通 DTO、Debug、模型或日志。二维码本机编码，不交给第三方图片服务。
HTTPS 与禁止自动跨 origin 重定向保护本机凭据正确性，不是 full-auto 权限门。

首版一个 active 账号，只将扫码本人及准确账号的 direct 文字准入原 Agent；不匹配的 peer 在原会话/Agent 效果前拒绝。
媒体、混合媒体及群聊不执行残余文字。账号与 peer canonical codec、持久 credential generation、原 session incarnation/fence 各保其义，换账号不接管旧请求。
配置/已授权、authenticated poll Ready、发送 Accepted、手机收件分别记录。`-14` 封闭新收发并要求重登。forget 只清本机登录/协议状态，不删历史或未结算义务，不假称服务端吊销。

可选 ingress receipt 在原 durable input 或控制 journal 处发布，lazy stream 返回不当 ACK；控制 claim/effect/result 使用原 ChatEventLog，效果不明保留 RecoveryRequired，禁止重放，不创造第二 inbox。
微信请求的原 response producer 捕获 context 和 generation，保留原 OutboundMessage fence。原 bounded send producer 的 typed receipt 区分 Accepted/Rejected/Unknown；缺 ret、超时或回执损坏不当成功，不自动重发。QQ/飞书未 opt-in 的路径保持原义。

原 factory 传入精确 WeixinDeliveryHandle，原 input pump route 在排队期间只存 receiver、该条消息的 context Option 和原 fenced template 数据，不占 response task、SessionStreamReceipt 或 delivery permit。submit、注册原 route 并启动/通知原 pump 后，无流准入返回 `Result<()>`。claim 后由原单一 pump 并行 poll 原 foreground/HITL 与回复 consumer，实际发送仍等待原 bounded producer；发送失败仍排空 renderer 并观察原 driver 结算。活动输入与显式 Steer 保留原 tracked live 注入语义；原持久输入和 supervised observer 已拥有该输入后立即 fenced ACK，明确等待原任务处理，不等待 drain 或追加输出 waiter；原 observer 继续记录真实 drain/recovery，无活动或 deferred 时交原 pump。原 slot 内的 RAII 发布准入覆盖 submit 至 route 注册，防止并发 pump 把正在发布的输入误判为无路由。
框架 `bind_outbound` 捕获 fence 不准入发送，`has_pending_work` 只观察原保留工作以保护 idle expiry；既有 Store CAS 按消息不可变保存 context，包括显式 None，`capture_message_context`/`send_with_context` 不回退到最近 token。
Session eviction 只观察保留的 input-pump 工作和 active foreground turn；该观察不会回收 service `JoinSet`。它的回收只由 coordinator 的 control 与 close 边界负责。


A+A2 使用 `SessionHandler::with_durable_ingress`、可失败的同步 `bind_durable_ingress(service_cancel)` 和无流 `admit_with_ingress(message, cancel, receipt)`。服务 child 在准入前绑定；stop 封闭 claim，取消准确原 foreground root 后再汇合物理结算。idle expiry 同时观察原 pump 和监督控制任务。

原 `ConversationInputPayload` 只新增可选 `channel_binding`：v1 channel、account/credential generation、准确 provider message/peer、原 opaque workspace product-data generation、必填 tagged Absent/Reference context 与 Chat/Resume 意图。Resume 保留完整 expected（含中央 Project binding）及独立 driver-continuation 开关。None 沿用中央 Domain-aware 原 digest；Some 的 digest 覆盖地址、文字、附件、Domain 选择和完整 binding。仅保存无密钥引用数据，不保存 token/fence。

原完整 journal 查询先按 provider 身份定位保存地址，再进行默认 workspace 设置。恢复初始化归原 ChannelRuntime 的 start/reconnect 任务组，等待同 epoch 的真实 authenticated-poll Ready，经同一 SessionHandler 建立新 fence。原 atomic claim 和 queue revision 在同一锁内复核严格 Persisted、所有 attempt 字段为空、未 drain、完整 pins 历史及已确认 durability；Deferred 或单独缺少 Started 均不是证明。已 claim/effect/send 或旧 owner 不明保留 RecoveryRequired。claim/effect/send 前在原 publication/scoped runtime lease 内核验当前 registry generation；原 provider 校验准确 message-context association，包括显式缺省，不回退最近 token 或当前 GUI。

恢复准入按 transport mutation epoch 与持久化 ConversationInputIdentity 只保留一个 owner。并发初始化任务可以看到同一份 DATA，但只有首次 claim 可以进入 SessionHandler；重复观察必须保留原 journal 事实，不得再次发送或调用模型。pending-work 观察必须无副作用：`has_pending_work` 不能回收或轮询 coordinator 的 service `JoinSet`；已完成 owner 的回收仍只在明确的 coordinator/close 边界执行。这样可避免 SessionHandler 准备准入时递归观察，并保持一次模型调用与一次发送结果。

Resume 只走原 Continuation bound-ingress 或 Planned `launch_task_run_resume`/wait，保留原 pool execution lease、Domain 策略与 expected；原控制 EffectStarted 确认后才 launch。Planned 的实际准入使用原 typed DriverAccepted 事实，不伪造 ReAct mailbox receipt。claim 前复用原 retirement gate 和运行时登记。排队只保存 DATA，不为每条输入增加输出或取消等待任务。

Framework ADR0007 提供原已提交前缀保证：只丢弃 active segment 最后无换行的未完整帧；完整坏帧、closed segment 坏尾及身份/序列错误拒绝 open。InputLifecycle 的 SyncData 确认后 claim 才能成功返回，pump 只从成功 claim 启动执行；完整 degraded claim 仍进入原 fold 并阻挡重放。因此未提交 incomplete tail 本身不应把已证明的 Persisted DATA 永久降为不可恢复。App 复用原 open/replay/pins 与 atomic claim barrier，不增加长度启发式、repair marker、旁路证明文件、validator 或物理 journal。这仅沿原 journal 支持的故障模型，不承诺任意外部历史删除/回滚，也不是编译或原生验收。


通知配置属于 EKO `channels.weixin.notifications`，按准确 definition/workspace 选择。原 occurrence 准入在同一 TaskRuntime journal frame 冻结目标；重复 occurrence 读取原绑定，包括历史 None，不能补猜当前配置。
目标含 account、credential generation、peer、opaque context reference、workspace、channel conversation 和 definition；原 occurrence/run 继续提供执行身份。
真实完成结果及产物引用进入现有 DeliveryLedger，通过 `apply_prepared_with` 把 typed delivery 事件提交到同一 TaskRuntime 物理 journal。确认持久的 claim/EffectStarted 先于网络 effect；内存投影不能代替物理提交。
完成与通知意图间崩溃只补未开始的意图；已有 effect 而无回执保 Unknown，不重新 claim。通知失败不修改原任务结果或重跑 prompt。

ApplicationServices 仍先构造原 scheduler，再发布 Arc<AppState>/ChannelRuntime。薄 binding 只保留 Weak，在原 callback future 内通过 Notify 等待绑定，并监听同一 scheduler cancellation；不因启动竞态失败普通 occurrence，也不新增等待任务或循环 scheduler。
send 前核对原已提交账号/generation、enabled 与冻结 context；不读取最近 token 或当前 GUI workspace 替换目标。

本地账号、目标或 context 校验在 HTTP 准入前失败时，原原因以 `(local not sent)` 保留为通知 `Failed`；不伪造平台拒绝码。原 sender 准入后不确定的发送必须返回 typed `Unknown` 并保留脱敏原因，不自动重发。两者都不修改已完成 TaskRun。

管理命令 `/trace`、`/analysis`、`/papers` 在原执行器返回处保留类型结果，再由旧入口显示同一次执行的文字。解析拒绝、读取失败、确认未准入、mutation 效果不明分别保留；读取失败保留无 mutation 的拒绝结果，不冒称原读取从未准入；只有原业务成功且 foreground 实际完成才报告 Applied。ProductData 的 `Admission` 证明准入拒绝，`OwnerClosed` 则是在准入后丢失结果观察，不能授权重放 mutation；物理 foreground 未结算仍为 Unknown。

控制结果由原效果 owner 在渲染之前给出类型结果：真实成功完成同一个控制 ledger；语法/无效果拒绝保留原身份；确认未准入则同 payload 持久 Deferred 并暂不 ACK；部分或不明效果先持久 Unknown 再回执 RecoveryRequired。Ok 文本和 EOF 不提供效果证明。输出观察者被丢弃后，原 supervised operation 仍持有同一个 lease。原生渠道命令显式采用 snake_case，与既有 endpoints/bridge 一致。

原已配置渠道启动完成后，同一 ChannelRuntime 调用 AppState 恢复入口，扫描全局与已登记 workspace 的原 journal。RunCreated 在同一帧冻结完整 OccurrenceIdentity 和目标；Completed run 可沿原 driver/ledger 补回缺失或未开始的通知 intent，workspace 物理 lease 由原 driver 保持至实际投递结算。Accepted 不重发、已开始 Unknown 不重新 claim；不重跑 prompt、不改 TaskRun 终局。身份或持久化缺口继续报告 pending 诊断。

## 影响

GUI 渠道页增加扫码卡片，定时任务页按原 definition 设置通知，历史只查询已解析原 run 的独立通知回执。
“已接受”不等于实际收件或已读；未验证无 context、重启、发送窗口或额度的服务行为，不承诺主动发送必达。
新依赖、锁和风险合同由中央作者按准确身份另行处理，不沿用旧接受证明新输入；不扩大 SDK accepted inventory 或修改其 build pin。

旧路径的普通 response 容量为 32，逐条等待输出可能使严格 cursor frontier 卡住，后续 `/reply` 留在 backlog。当前原 pump 数据 route 与短准入 stream 机制针对这个结构性反例；不是仅增加独立控制槽，也没有新 inbox。超过 32 条及后续 `/reply` 的压力回归、编译和原生检查尚未运行，不能宣称该路径已验证闭合。完整 Outcome 仍需这些证据。
本地静态源码、fixtures、工程构建、consumer、真实 GUI/手机、main CI 与发布分别结算；官网仅由中央 owner 在获准来源冻结后镜像，不在本次部署。

## 验证

新增管理命令回归定义经过真实 SessionHandler：覆盖 analysis 参数拒绝、原 analysis supervisor 关闭、trace store 缺失、ProductData 准入关闭、真实 native 创建/删除错误及成功创建 paper。关闭并重开原 journal 后，同一 provider ID 保持拒绝/恢复/成功事实；恢复的 source 文件用于发现重复 delete，source 数量用于发现重复 create。所有定义仍未执行。

控制 fixture 关闭并重开真实 SessionHandler 和原 journal 后，重放 Unknown 结果及成功的只读控制。24 条 fixture 经过真实 SessionHandler 与持久 submit 入口，观察实际 tracked mailbox 输入，其余有序输入仍归原 frontier；实际取消后检查原结算/恢复记录，不声称 24 条同时进入 tracked mailbox。40 条 fixture 保留原 pump election，全部输入与手机 reply 均经 SessionHandler；provider cursor/保留响应压力由独立 framework poll fixture 覆盖。这些都是尚未执行的测试定义。

现有源码包含协议状态、脱敏、游标、控制重放、目的地冻结、物理 journal durability、Unknown 恢复及 GUI 请求身份的测试定义；定义存在不表示执行通过。
需覆盖 32 槽饱和+后续 `/reply` 的真实有界准入路径，并完成适用编译/测试/feature/示例与精确依赖门禁。
框架 demo38 保留 QQ/飞书真实示例，默认 AgentChannelHandler 缺 durable ingress opt-in，不补假 ACK；注释修改不算示例编译。

本候选无编译或原生验收。用户仅在最终准确 GUI 版本完成真实扫码、数字确认、单聊/HITL、重启及指定 cron 通知的手机收件检查。开发 fixtures 不连接真实账号，不以 mock、HTTP200、webhook finished、截图或源码存在替代平台接受和实际收件。
参见 [配置指南](../configuration.md)、[任务面板](../guides/gui-task-runtime-panel.md) 与 [ADR0053](0053-im-cron-gui-control.md)。

新增 A2 测试定义覆盖 None/Domain 原 digest、必填 context/generation、地址/binding 篡改、context 无损转换、clean journal 重开与唯一 claim、曾 claim 的 Deferred 排除、atomic claim 前服务取消、未提交 incomplete tail 经其它 boot scan 后仍可正向恢复、完整坏尾拒绝隔离，以及超过 128 个缓存 stream／4096 条 replay record 的完整发现。原 40/24 个真实 SessionHandler 定义已改为无流准入和 canonical sender，仅经注入 Store 准备显式无 context 的协议关联。全部定义仍未编译、未运行，不替代原物理 owner 合同及工程验收。

控制输出适配在内部区分 typed immediate response DATA 与 legacy stream；无流准入不轮询 opaque stream 来推断回执或输出。原压缩/reset/extraction/management 监督 owner 经同一 delivery handle 发送最终文字，即时控制在原短调用内发送。普通输入、恢复及控制回复均保留原 provider message ID 的 reply_to。

长控制源码现将 reflection、workflow 执行交给原 coordinator service JoinSet，不增加 foreground 门控；browser confirmation 和其它 Extension 保留原 foreground lease，在原 foreground supervisor 中执行同一 operation body。legacy surface 仍直接等待同一执行体。准入不等长效果，原 owner 继续持有控制 journal lease，仅按真实结果报告并发送最终回复，task 登记成功不发布 ControlHandled。idle 观察回收原任务组并保留失败；Stop 汇合该同一任务组，close 观察者被丢弃后仍可重试。不新增队列、dispatcher、store、public API、审批服务或原生控制策略。新增真实 SessionHandler 测试定义覆盖等待中的 reflection、原 workflow Graph 完成和 browser consequential confirmation，均未执行。完整 A2 仍须真实 Ready→driver→Accepted-send consumer fixture 与两正两负崩溃场景，不以 journal-only 或伪 sender 证据替代。

监督控制的原观察器保留已经 journal 确认的 disposition，直至该同一 owner 的回复 future 真实结算；回复完成、drop、EOF 不提供任何事实。sender 背压仍留在既有 pending frontier 内，不另加许可、门控或队列。native owner 在执行体首次 poll 前标记回复归属，覆盖任务先于准入调用者恢复执行的情况。等待原回复及缺少原事实的回归源码仍未执行。

Ready 初始化把当前 live route 观察和从未 claim 的证明交回同一私有准入路径，不用独立预检查把本轮活 owner 降成 Unknown。同一已验证输入的并发 route publication 复用原 reply slot。已完成/取消历史在初始化和 provider replay 时都保留原终态。consumer 测试在下一次冷启动核原 completed outcome，不只检查模型调用数为零。原 atomic claim 后，pump 保留该 route，直到既有回复发布准入全部结束，防止竞争初始化在已消费 route 后补入多余 route；这只补全原发布屏障，不另建队列或生命周期。冷恢复测试源码并发运行两个原初始化任务，仍未执行。

原 service wrapper 取消已绑定 child 并等待同一 coordinator 的恢复/pump 任务组，不只依赖外围 managed Stop；inner session close 先撤回原 IM HITL 再等准确 foreground。取消 close 观察者仍保留原任务组供重试。Start 同时捕获默认 runtime 与 opaque product-data generation，同 ID 重新关联/重建不能静默替换默认绑定。仍存活的原 route 保留 typed Deferred 语义，仅重建 route 走严格 A2 证明；发现失败不当身份不存在，阻塞地址保留原事实但不遮蔽其它独立安全 stream。对应回归定义仍未运行。


### 扫码等待中的单次超时

参考主根内 ZCode 的串行扫码轮询：一次长轮询超时不等于二维码过期。原 AuthOwner 仅对框架已类型化的 `Io(TimedOut)` 继续观察同一 challenge，保留原 250 毫秒间隔和 5 分钟绝对 deadline；其它协议/认证失败保持 failed。已消费的验证码不因响应超时自动重发，后续只查询同一二维码状态。取消、配置 revision、实例 epoch、applied revision、credential generation、intent 和原 slot 的提交检查继续成立。

框架只保留请求超时的通用类型事实；串行重试、过期和凭据发布仍由 EKO 原 owner 决定。不增加 Core 错误枚举、生产公共 API、store、运行时或新的登录状态机。ZCode 的旧版 GET QR、缺少数码校验和通用 `-14` 重试并非本项目采用目标；当前 POST QR、完整身份、数码/redirect 及失效停止准入保持。

新增回归通过原 begin、认证 producer、实际回环 HTTP 与 owned 配置提交：覆盖单次超时后同 challenge confirmed、永久错误不重试、取消后迟到确认、连续超时不重置过期、凭据变化及原 channel epoch 变化拒绝迟到提交，以及数字码不重放。夹具只缩短自身 deadline，测试进程必须显式设置自有 `EKO_DATA_DIR`，不修改用户数据根。2026-10-26 epoch11 已取得旧策略真实失败与最终7/7通过；框架响应头前/响应体中的超时另有2/2通过。最初编译错误和代次夹具6/7失败保留，正式代次替换夹具修正后重新执行。本机回环通过不代替原生扫码和手机收件。

### 混合会话日志与私有分派的实现边界

会话物理 journal 按工作空间和会话寻址，正常包含 DATA、普通 turn 及多个控制 root。控制适配器在原锁内读取真实物理前缀并先做完整记录校验，只向原严格 reducer 投影准确的控制 root；checkpoint 记录该次扫描的真实物理 head。其它 root 的合法事实不属于该控制投影，物理损坏、当前 root 的类型错误或缺失前缀仍拒绝；不新增 journal 或持久化权威。

长控制的公共校验和分支选择保持原顺序，各私有执行分支返回 boxed future，由原调用链直接 await，避免未选中分支的巨大 future 聚合到公共分派帧。原 coordinator、foreground、控制 claim/effect、冻结 context、发送回执及关闭 join 继续持有原效果；不增加线程、队列、调度器或提早 ACK。该两项实现已静态审查，epoch12 的默认栈、原13项消费者与新增混合日志回归仍待执行，不把框架或扫码的局部通过用于结算这些范围。
