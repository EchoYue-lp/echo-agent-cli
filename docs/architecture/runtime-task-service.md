# RuntimeTaskService 适配决策

状态：已采纳

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
  pause/cancel intent、GUI/TUI/CLI/channel 投影。
- adapter：无损转换 `PlanTask` extension，调用 framework pure transforms，并把 task 变化、
  EKO summary 和 claim-bound review 作为一个 `append_batch` 提交。

`events.jsonl` 是 EKO 权威。`plan.json`、run state 和 Todo 是可重建投影。projection refresh
降级发生在 journal 已提交之后时，调用方保留 typed mutation outcome，后续读取自愈；
batch outcome unknown 则 fail closed。依赖失败只通过 `DagExecutionState` 派生到 Todo read
model，绝不写 descendant `Blocked` 事实。

暂停与取消遵循 `Cancelled > Paused`。EKO 不新增 task-level Paused UI 状态：active task 的
framework Paused 在文件 adapter 投影为 unclaimed Pending，Run 保持 Paused；resume 不增加
retry。取消先持久化 Run/Task Cancelled，再触发 driver token。

所有 app-core async TaskRuntime 文件 I/O 通过进程共享、固定上限的
`TaskRuntimeBlockingAdapter` 进入 `spawn_blocking`。Drop 只释放内存 registration；终态文件
写入由可 await 的 driver/supervisor 路径负责。

## 取舍与影响

- EKO adapter 不再拥有 executor、validator、retry loop 或 descendant traversal。
- Review、summary 和 touched-files extension 只有 exact claim CAS 成功后才发布；stale claim
  的候选结果直接丢弃。
- Subagent durable recovery 使用 revision、attempt、physical execution id 和 TaskStarted
  线性化边界；合法的 restart handoff 可复用，迟到旧 release 不可越过新 claim。
- Surface 层的 blocking 调用将在 `taskruntime-blocking-surfaces` 迭代中接入同一 adapter；
  该工作需在 scoped-control/channel 分支合入后执行，避免并行 worktree 交叉修改。

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

`echo-agent-examples` 用于验证 framework public facade，不应引入 EKO 文件 journal、review
或 worktree policy。`echo-website` 是静态展示站，不依赖 agent runtime。因此本决策不修改
framework examples 或 website。
