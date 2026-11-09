# ADR 0053：GUI 管理已有 IM 与 cron owner

产品决策已接受。当前源码已合流原 IM/cron owner、HITL 投递身份与桌面生命周期接线；源码合流不代表完整 Outcome、Rust 编译或原生桌面已验收。

## 状态

已采纳

## 背景

GUI、仍保留的渠道入口和定时工作必须共享原 EKO owner。ChannelManager 拥有 transport/handler 结算；SchedulerRunner、CronTaskStore 与 occurrence journal 拥有调度事实；ConfigState owned save 拥有配置发布。GUI 不能用新 store 或执行器取代这些权威。已批准的跨仓收敛 Design 与 Plan07 定义产品范围，ADR0050 定义框架与应用分层。

原 HITL request_id 在 provider future 结束后释放并允许重用，仅靠 ID 无法区分晚答与新请求。框架请求/响应类型保持不变，Selection context 和 browser session 身份也保持原义。

## 候选方案

1. 继续只按 request_id 回复：晚答可能回答复用同一 ID 的下一请求。
2. 增加框架字段或中央审批 store：会把 EKO 展示策略下沉框架，或复制原 future/pending 权威。
3. 在 EKO 按每次实际投递配对，保留原 provider future：采用。

## 决策

EKO 的 `pair_request` 保留已有 request_id，仅在缺失时补一次；每次实际投递生成新的非空 `response_target`。`HitlRequestIdentity` 包含原 ID、target、kind 和 `HitlRequestScope`。scope 携带可用的 workspace/host generation、conversation/root turn/turn、run/task 及 channel/sender incarnation 坐标。独立 provider 缺少的坐标保持缺失，实际 GUI/渠道 owner 提供原坐标；不挪用 Selection context 或框架 session_id。

Provider 在真实 request future 内通过原 ForegroundTurnControl 和 ChannelActiveTurn 记录的 Weak 只读引用解析坐标，不依赖 framework stream spawn 保留 EKO task-local，不强持有应用 owner 形成环，也不改绑当前页面工作区。同一原 owner 提供 continuation 的当前 turn；owner 缺失或已结算时，在插入 pending 前拒绝。Pump 准入也发布到原 channel map，并由原 foreground lease 保留既有 removal receipt，直到物理结算。

普通 GUI、恢复 TaskRun 与已注册 browser Approval 走同一个 Tauri provider 的直达 request future。EKO paired provider 入口只验证、消费已有 pair，不重新生成 target。scoped GUI+IM dispatcher 在 clone 前配对，让原 provider futures 共享 pair，并只选一个有效人类答复。Timeout、Deferred 和 provider 不可用不能抢赢其它人答；共享 deadline 取请求 timeout 与 300 秒的较短者。旧 generic provider 注册保持可用。

原 GUI pending map 改为按 target 定位。三个答复 IPC 在 remove 前核对 `request_id`、`response_target`、`request_scope`、`request_kind`、当前答复类型和原 active turn 坐标。Input 不能为空白，Selection 必须属于原 options，Approval 只接受既有 scope，且都在原 pending-map 锁内验证后再 remove。非法输入保留等待项；sender 失败返回 expired；send 成功只返回 submitted，不冒充 effective winner。取消保留 workspace/conversation 边界。reservation 释放只投影移除同一身份，不新增审批终态或 store。

新配对 request/release 的 journal envelope 保持原 root、使用当前 turn，并拒绝不一致的 stream 坐标、ID 或 kind。请求事件携带可缺省的 EKO `identity`；缺失时序列化不补字段，以保历史 payload/hash 身份。回放不生成 target，也不激活缺 target 的历史请求。手写事件适配沿原 journal envelope、handler、pending、ChatPanel 和三个生产 useTauriChat 调用保留原身份。点击使用该请求的 scope，不借当前页面重绑。

IM 在普通 chat、steer 和管理准入前消费 `/reply <response-target> <answer>`。只有当前 front 的 target/kind 匹配才可回答。缺失、过期、非 front、其它会话的 target 明确拒绝；有 pending 的裸文本只提示目标。无 pending 的普通文本仍可聊天。Input 保留 answer 后缀的 UTF-8 与换行；Approval/Selection 继续使用原解析，取消为 `/reply <target> /cancel`。裸 `/cancel` 只提示带 target 的回复形式；`/stop` 与 `/reset` 仍走原控制路径。原 reserved_ids 持有至原 future 结算。

原渠道实现已归位 app-core `channel_runtime`，保留的 bin 入口仅转发；随同移动的仅为原文本命令适配。ApplicationServices、Tauri 与独立 channel 入口使用同一 ChannelRuntime、原 ChannelManager 和原 coordinator/session/pump owner。GUI+IM factory 只持有 Weak host/state，复用原 scoped dispatcher、GUI pending map 与 IM FIFO；无桌面 factory 时仍走原 IM direct provider。原 pump driver 将 prompt broadcast 转发到既有 render channel，观察通道丢失只撤回 IM provider，不阻止仍可回答的 GUI。

ConfigState 在原 owned save 成功后，以同一 write guard 发布已提交配置和 opaque UUID revision。provider/model/full-config/channel save 共用原 owner；此 revision 不借用 transport mutation_epoch，也不混同于构造现有 adapter 时应用的 revision。保存不启动连接。Start/reconnect 检查 desired revision 与原实例身份；Stop 检查已投递实例的 epoch/applied revision，因此全局模型保存不会使旧连接无法断开，旧 Stop 也不能关闭后继实例。GUI 只得到凭据是否存在；省略保留、明确 clear 才清除，Debug 隐去内容。Unknown/closed 观察仍为未知，本机 webhook listener 就绪不等于平台可达。

原 pump JoinSet 留在原 coordinator registry，由一个 owned producer 借用并排空；丢 waiter 不会 abort 原任务。单任务失败保留为诊断，同时继续 join 其它任务；已物理结算但含失败与物理状态未知分别处理。只有原物理收据和 foreground map 均确认结算，显式重连才可复用同一 coordinator 及 incarnation/retirement gates；不自动重放或替换 owner。

ApplicationLifecycleOwner 在 normal_drained 之外持有应用专用 channels slot。Phase1 封原准入，Phase2 观察同一原句柄；stop Err、Unknown 或 cleanup panic 保留句柄并阻止 Agent/ToolManager/pool 依赖提前关闭。已结算任务失败保留诊断但允许物理释放。真实 producer post-commit 收据与原 transport 变化通过 Weak 唤醒原 single-flight continuation；重复 wait/通知不构成物理进度或重复 stop 的许可，合法重试必须观察到原 manager/session 事实变化。generic external-owner 合同保持不变。

窗口 X 先 prevent close 再隐藏，失败恢复显示并报告；托盘显示恢复/focus，明确退出走原两阶段 owner。桌面最终结算边界只订阅同一 receipt 的实际 final 发布，在事件循环返回后仍保持 Tokio 存活；其它调用方 bounded wait 的观察语义保持。缺 continuation 或观察关闭不能制造 final，已结算但含失败的 final 仍如实报错。Unknown 永无真实进展时，真正退出可能持续等待，而不会谎称物理完成。

Cron edit 使用原捕获的 definition/revision；手动执行无损投影原 occurrence/attempt、typed cause、物理结算、terminal apply receipt 与 journal outcome，callback result 字符串保持 opaque。历史读取原 bounded journal page，cursor 以 opaque 字符串无损传递；JS sequence/floor 用字符串。retention/generation 错误保留页面直到明确刷新。run 链接从 occurrence 的 canonical conversation 查询已有事实，只打开精确已有 workspace/conversation/run，不借当前页面、不解析 callback 字符串、不创建新 run。

不新增平台、daemon、SQLite、崩溃重放政策、Agent 权限门、任务图、store 或执行器。Depth ADR0052 与 SSH ADR0051 保留各自来源和验收边界。


保留的独立 channel 入口在全部真实启动失败时沿原清理返回错误；GUI 无配置仍为空态，单个渠道失败不阻止另一成功渠道。paired GUI 投递失败只撤回该 observer，独占 direct GUI 路径保留原失败投影。Pump prompt 背压在原 bounded queue 等待容量，同时继续 poll 同一 driver；观察关闭或 lag 只撤回当前 IM 请求，不永久关闭 session；同一 drive 继续观察后续 prompt，在本轮 renderer 仍不可用时立即撤回 IM 资格，避免下一请求无人观察直至 timeout。GUI 仍可回答，下轮独立 IM renderer 仍可正常使用。

## 影响

应用获得独立于可复用框架 ID 的投递身份，没有新增 queue、任务图、store 或执行器。保留的 CLI/TUI 只维护已有 literal/exhaustive match；identity 缺失时保持旧字节，GUI release 投影不新增 JSONL record 或命令。

合流适配有 focused Node before/after 与 Rust 回归定义；当前组合源码尚未由 Cargo 编译。Rust/GUI 门禁、真实 Rust IPC 导出、原生 GUI/browser/IM、实际双端组合、fresh 精确依赖风险资格及精确 main CI 均保持待验。不可变旧身份验证与其 warning 单独记录，不重标为本组合源码证据。官网由协调网站 owner 同步。本 ADR 不宣称真实平台投递或完整产品验收。

## 实现参考与取舍

[Tokio graceful shutdown](https://tokio.rs/tokio/topics/shutdown) 区分通知停止与等待已接收任务完成；[Tokio JoinHandle](https://docs.rs/tokio/latest/tokio/task/struct.JoinHandle.html) 明确 drop handle 会 detach task。因此 EKO 在原应用 owner 保留原句柄并观察真实结算。未采用扩大所有 generic external-owner 合同的方案：渠道生命周期组合属于应用层。本地仍需要这些保护，是为了避免应用清理 bug 或误丢已接收工作，不新增用户交互权限门。

## 2026-10-27 手动执行的并发观察隔离

SchedulerPanel 对每个原 scheduler task 独立保留 pending 与手动回执，防止同时运行 A/B 时较早返回的一项覆盖另一项结果或解除其按钮禁用。同步 pending ref 只限制页面上的同任务重复操作；不同 task 可并行，已结算后再次手动运行仍交原 runner 创建独立 occurrence。GUI 不产生 occurrence、TaskRun 或完成事实。

页面关闭只撤销该页面的观察；已准入的 callback 仍归原后台 owner。旧页面迟到结果不写新页面、不发全局成功提示、不再次刷新；重开从原列表／历史读取。并发列表刷新使用最后请求序号，旧响应不覆盖新定义。IPC 响应丢失只表示回执未确认，提示读取原历史，不自动重发，也不把它投影成运行失败。

这个切片沿用已有 UI 临时状态，未新增 store、执行器、重试、框架 API、凭据或发送行为。现有 examples 没有此 GUI 面板的独立示例，故不新增代码示例；官网对“定时任务管理”的能力说明未改变，待实际合流由官网 owner 核对源指纹。定向组件验证不能代替 native、真实 IM／微信、完整矩阵或 main CI 验收。
