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
