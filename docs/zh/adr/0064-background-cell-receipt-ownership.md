# ADR 0064：后台命令回执与资源所有权

状态：实施中，验收证据单独记录。

## 背景与取舍

普通 Chat 和 TaskRun 原先在持久 Started 之后、返回工具回执之前等待全进程 shell 许可。长命令占槽时，调用方可能收到 timeout，而后台 cell 已产生。提高配额或 timeout 无法消除这种所有权错误。

## 决策

产品仅向框架原 Queued runner 注入 `ProcessExecutionGovernor` 的 shell=1 政策；移除两个真实主路径在回执前等待许可的逻辑。Started 持久失败时 abort 原 reservation，不执行命令。排队期限仍从 accepted_at 起算。

原 `TaskTracker` 持有单次 launch 到 terminal observer 接管的完整交接。`launch_observations` 仅保留回执观察结果，不调度进程；完整 workspace/conversation/run/root/turn/execution/call 身份的传输重取加入同一 producer。改变输入但复用身份报错；有意重复命令必须使用新的 call_id。不以 command hash 作为业务去重键。该回执观察限定在当前服务进程，不承诺跨重启重放命令；重启沿原持久 Started/Finished 事实与恢复路径判断未知结果。

关机封口复用 `launch_observations` 的原临界区，将关闭检查、producer 登记和 `TaskTracker.close()` 串行化。已通过关闭检查的原 producer 必须先完成登记，关机才开始等待；否则 TaskTracker 允许 close 后登记的行为会使关机漏等。锁在 close 后立即释放，不跨异步等待，producer 到 terminal observer 的交接继续由原 owner 保有。

前台 Turn 取消和已接纳 cell 的显式 stop/run cancel/shutdown 分开；输出与 Finished 持久失败保留原 observer 债务及产品许可，不能伪造结束再放行新进程。字段 started_at 仍表示接纳时间，Prepared 不表示 OS 已启动。

## 分层与验证

通用准入、生命周期、物理结算和资源保有归框架；配额、作用域和产品事件持久化归应用。见框架 ADR 0107。没有新队列、supervisor、Outbox 或 SQLite。

验收使用普通 Chat/TaskRun 占槽回归、回执重取与身份反控、持久失败、明确运行取消，以及 producer 登记前暂停的 shutdown 竞态控制。原微信业务 101 由微信恢复计划处理。源码与受控回归不代表真实 App 或 main CI 已通过。

## 2026-11-08 补充：Verification 只拥有逻辑任务许可

真实回归发现父 Verification 持有唯一进程 shell 许可并等待后台 cell 时，cell 会停在 Queued 直至原期限耗尽。保留每 run 的逻辑 Verification 串行和 write／LLM／Subagent 约束，删除父模型调用对物理 shell 的持有。物理容量仍是原 ProcessExecutionGovernor.shell=1，由每次实际原生命令取得。

App 使用同一个 acquire helper 实现中立 NativeExecutionResourceAdmission 与既有 CommandCellResourceAdmission：前台 shell、run_code、run_skill_script 原生探测／执行在 dispatch 前取得；后台只在原 Queued runner 取得。正常 factory、writer child、注册／新实例、Plugin 及最终 shared ToolManager 替换后安装同一 provider。重复安装只替换同一能力引用，不包装 executor，不弱化 scoped Arc／项目／来源校验。

新许可只送往本次 SandboxCommand／原 direct child owner，不写回长寿命 context。取消／超时／观察掉线都不是物理完成；清理未知时原 owner 与资源保持，实际 wait／原进程组消失后才核销 exact fence。原 timer 不重启；后台 accepted_at 起算的 deadline、回执、TaskTracker 与持久观察债务保持。

用户 PTY／MCP、项目控制 I/O 与用户扩展配置不受本次自动 Agent 容量 gate 扩张。验收保留原 5 秒真实 Verification 用例，分列真实 OS 回收与模拟 Unknown 资源控制；最终冻结 consumer／依赖风险门禁、App 与 main CI 继续分别验收。

### 支持范围与显式来源迁移

App scoped registry 执行最终权威 Some 政策，输入 ToolContext.provider=None 不能降级它。该政策复用框架唯一 native→cell 转换；能力要求贯穿原 command，最终 executor 与 fallback 必须兑现 physical ownership。Mac 默认／已证明的 SSH 与当前 Unix direct 正常路径的回归仍阻断采用。

正常 factory 为本地 Skill 定义绑定 local(Some(local_sandbox))，child／Plugin 保留同一来源；公开 Local Windows 原本缺 Job Object 执行能力。显式 `register_descriptor_with_source` 的 local(None)、custom executor 或 direct 集成则仍保留其定义来源；未证明平台／后端在有效 Some 下新增 Unsupported，是可见合同变化，不能称纯内部无行为影响或全部平台修复。迁移应让原适配器提供真实 owner／guard／Unknown／终结证明及对应能力声明；不得按平台名自动 true、批量标记 mock 或偷偷换 host executor。最终未使用新政策的框架路径保持旧行为，PTY／MCP／控制 I/O 保持原路。未验平台与 live 资格单列保留。

旧 35 项受控回归的 service helper 从无 sandbox 前置改为生产同类的真实 Local owner，保留原断言、时限、global1 与来源／策略绑定负控；这只是测试前置修正，不能替代实际 35 项结果或平台行为证明。

### SSH owned group 与明确运行时前置

有效 Some 的远端 deadline controller 由 Python3 stdlib 在同一个原执行 channel 中 exec 接替原控制器角色，不叠第二 supervisor。必须在 payload 前实际探测 `Popen(start_new_session=True)`、PGID、`killpg`、signal 和 monotonic；不能只凭命令名／版本，CAS 使用 Python3 也不证明所有 SSH 目标都有它。缺少运行能力时返回绑定本次 nonce 的明确未启动凭据；None 仍沿旧 POSIX 路径。

原 owned group 的正常 root 退出／EOF、非255 SSH退出、`killpg` 的 EPERM，都不能单独结算。单个原 monotonic 截止时间覆盖启动探测、负载、后代组等待与清理，不因 root 退出或进入清理重置。只有实际原组已结束，或 payload 明确未启动，才能产生本次 nonce 的内部终结凭据。客户端在 stderr 读取阶段有界分离完整尾部；原 payload、输出限额及计数保持，凭据缺失／截断／不匹配保留原 Unknown owner 和资源。

纯 sh 无此运行能力的目标在 Some 下会收到显式 Unsupported，是本次可见边界；不能无依据声称其已保留。当前正常已证明目标／fixture 若缺少运行能力并发生新退役，仍阻断采用并需证据和适配；不以新前置绕开这些正控。当前资料只支持实现选择，不授 SSH/Windows live 资格。
