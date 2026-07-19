# M7 Subagent 结构化结果与真实完成判定

## 目标

M7 将 Subagent 的“执行已经结束”和父任务的“需求已经满足”拆成两个独立事实。框架记录执行终态与结构化证据；EKO 根据 PlanTask 声明的必需产物、验证项和未解决问题决定 task/run 是否可以进入 `completed`。模型文本中出现“完成”不构成完成依据。

## 业界依据

- [Claude Code subagents](https://code.claude.com/docs/en/sub-agents)：Subagent 使用独立上下文执行，只把结果摘要返回主会话；已完成 Subagent 可按稳定 agent id 恢复，恢复时重新进入 running，而不是沿用旧终态。
- [OpenAI Codex exec events](https://github.com/openai/codex/blob/main/codex-rs/exec/src/exec_events.rs)：非交互消费者读取明确的 item started/completed/failed 和 turn completed/failed 事件，不从最终自然语言反推生命周期。
- [LangGraph persistence](https://docs.langchain.com/oss/python/langgraph/persistence)：checkpoint 保存每一步的持久化事实，恢复从已完成节点继续，而不是重放整段工作。
- [Temporal Activity definition](https://docs.temporal.io/activity-definition)：完成事实可以只观察一次，但执行可能因故障重试多次；返回值进入事件历史，写操作仍需稳定 identity、幂等或可验证 postcondition。

跨系统共性是：执行终态由 runtime 产生，结果以可持久化结构返回，恢复复用已记录事实，业务完成条件由上层工作流验收。EKO 沿用这一分层，不增加 Planning/AwaitingApproval/Ready 等运行状态。

## 现状审计

- 框架 `SubagentResult` 已有 output/summary/artifact path/cancelled/usage，但没有统一 status、verification、remaining_work 和 touched_files；timeout 仍以普通字符串错误传播。
- 取消路径会先发 `DispatchCancelled`，随后 outer dispatch 又把 `Ok(cancelled result)` 发成 `DispatchCompleted`，终态不唯一。
- EKO `TaskExecutionSummary` 已有 files/verification/failures 等相邻字段，但成功路径把 PlanTask 的 verification 要求直接复制成“已验证”，没有执行证据。
- `SubagentReleased` 只保存 bounded summary，恢复后只能复用字符串，不能重新执行完成门禁。
- `run_dag` 只看 todo 是否 Completed；unattended agent 流结束时还会把未收敛的 run 直接标成 Completed。

## 框架与应用边界

### `echo-agent`

- 提供通用 `SubagentStatus`：`completed/failed/cancelled/timed_out`。
- 提供结构化 result：summary、artifacts、verification、remaining_work、touched_files。
- status 由 executor 覆盖，不能信任模型自报；timeout 使用 typed `AgentError::Timeout`，cancelled 不再追加 completed 终态。
- artifact 引用携带 path/kind/bytes/SHA-256/producer execution id/availability；框架在可解析到真实文件时计算 bytes/hash。
- verification 区分 observed 与 reported；只有工具事件观察到的成功检查可作为 EKO 的必需验证证据。
- terminal SubagentEvent 携带同一结构化结果，供任意框架消费者投影。

### `echo-agent-cli`

- `PlanTask` 声明 `required_artifacts`，现有 `verification` 继续表示必需检查。
- `TaskExecutionSummary` 持久化一个权威 `SubagentTaskResult`，不再把要求复制成结果。
- 完成门禁检查：status=completed、summary 非空、remaining_work 为空、无 failed/not_run verification、每个 required verification 有 observed pass、每个 required artifact 可用且带 hash/producer id。
- 门禁失败先走已有 review/fix retry；达到上限后 attended run 暂停、unattended run 失败，不允许父 run completed。
- `SubagentReleased` 持久化 bounded structured result；resume 复用结果后仍重新执行相同门禁。
- GUI/TUI/CLI 消费同一 terminal result；展示 timed_out、remaining work、verification 和 artifact 状态，不自行推断完成。

## 结果合同

```json
{
  "status": "completed",
  "summary": "bounded parent-facing result",
  "artifacts": [
    {
      "path": "reports/result.json",
      "kind": "report",
      "bytes": 1234,
      "sha256": "64-hex",
      "producer_execution_id": "task_id:attempt",
      "available": true
    }
  ],
  "verification": [
    {
      "check": "cargo test --workspace",
      "status": "passed",
      "details": "bounded result",
      "source": "observed"
    }
  ],
  "remaining_work": [],
  "touched_files": {
    "read": ["src/a.rs"],
    "written": ["src/b.rs"]
  }
}
```

Subagent prompt 使用 fenced JSON 返回上述字段。缺失结构化块时只保留 UTF-8 安全 summary fallback；verification/artifacts/remaining_work/touched_files 不从散文猜测。runtime 始终覆盖 status、producer execution id 和可计算的 artifact hash。

## 完成门禁

task 可进入 Completed 当且仅当：

1. subagent terminal status 是 `completed`；
2. summary 非空；
3. remaining_work 为空；
4. result 中没有 failed/not_run verification；
5. PlanTask 每个 verification 要求都匹配一条 `observed + passed` 证据；
6. PlanTask 每个 required artifact 都匹配 available artifact，且 SHA-256 与 producer execution id 完整；
7. review gate（适用时）通过；
8. run 中没有 Failed/Blocked/Pending/Running task 或 unresolved recovery blocker。

Skipped 是显式放弃的任务事实，不伪装成成功；存在 Skipped 时 run 只能在明确降级记录无 remaining work 后完成。M7 不新增 `partially_completed` 主状态，部分完成通过 failed/paused 终态与结构化 remaining_work 表达。

## 失败与恢复语义

- failed：保留 summary/evidence/remaining_work；可重试时生成同 task id 的下一 attempt。
- timed_out：独立 status，不自动当 success；已持久化 artifact 和 completed tool facts 可复用。
- cancelled：只发 cancelled terminal，不进入 review/complete。
- process restart：按 execution id 读取 SubagentReleased structured result，重新执行 deterministic completion gate，不重新派发已经完整结束的 subagent。
- indeterminate mutating side effect：沿用 RecoveryBlocked，必须人工确认 retry/skip，不因 Subagent 文本自称完成而清除。

## 验收

- completed result 缺少 required artifact、artifact hash、producer id 或 required verification 时 task/run 均不能 completed。
- subagent timeout、cancel 和 failed 各自产生唯一且不同的 terminal status。
- 单 subagent 失败、多 subagent 部分成功、synthesis failure 和 resume-after-complete 均保留结构化结果与 unresolved failure。
- resume 复用 completed subagent result，并重复相同门禁；不重复 subagent 副作用。
- unattended 流结束但 plan 未真实完成时不得直接 completed。
- GUI/TUI/CLI 对同一 terminal fixture 展示相同 status、summary、verification、remaining_work 和 artifacts。
