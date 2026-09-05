# ADR 0038: Unified TaskRun and Subagent Execution Model

- Status: Proposed
- Date: 2026-09-04
- Owners: `tasks/task_runtime`, `agent_pool`, `subagent`

## Context

One EKO goal must span requirement breakdown, planning, implementation, review, and delivery,
while allowing the implementation phase to add more steps. One `TaskRun` and one revisioned Task
Graph should carry that lifecycle; phases must not create child TaskRuns or separate task queues.

The meanings of `TaskRun`, `PlanTask`, `SubagentRun`, direct `agent_tool`, nested dispatch,
`Sync`, `Fork`, and `AgentPool` are currently easy to conflate. If Subagents can call
`agent_tool` again, fan-out is unbounded, and several local semaphores do not express one EKO
product limit.

## References

- Claude Code's official Subagent documentation separates the running concurrency limit from
  nesting depth and rejects new dispatches when capacity is exhausted:
  <https://code.claude.com/docs/en/sub-agents#concurrent-subagent-limit>.
- EKO ADR 0003 records the boundary between Agent, Task CRUD, background handles, and Plan
  artifacts.
- EKO ADR 0015 establishes `TaskRun -> PlanTask -> SubagentRun` as the sole relationship graph.
- `echo-agent` already provides `RuntimeTaskService`, `NestedDelegationPolicy`, and
  `KeyedExecutionAdmission`; the Subagent portion of EKO's current
  `EkoExecutionLimits`/`PROCESS_EXECUTION_GOVERNOR` is an application duplicate that should
  converge on the framework primitives.

## Decision

1. One active user goal has one `TaskRun`, one TaskRuntimeStore, and one authoritative graph.
   Phase expansion commits a new revision of that graph; it does not create a nested TaskRun,
   child queue, or second Todo/TaskRuntime authority.
2. `PlanTask` is a node in the global queue, while `SubagentRun` is one execution attempt for
   that node. Retries remain attached to the original PlanTask and TaskRun.
3. The primary agent retains `agent_tool`. Subagents do not register `agent_tool` or
   `task_execute`, and runtime rejects `delegate_depth >= 1` with
   `delegation_depth_exceeded`. Only the primary-to-one-Subagent level is allowed.
4. Subagents may return decomposition proposals, evidence, and results; the primary agent and
   TaskRuntime's existing authoritative APIs commit graph revisions.
5. EKO uses `max_concurrent_subagents` for the process-wide count of running `SubagentRun`
   instances, defaulting to `5`. This product value is injected through a shared execution
   admission based on framework `KeyedExecutionAdmission` and is shared by TaskRuntime, direct
   `agent_tool`, Sync, Fork, and any retained Teammate path.
6. Framework `RuntimeTaskServiceConfig.max_concurrent_subagents` remains the per-runtime
   scheduling width for standalone framework consumers, while
   `SubagentExecutorConfig.max_concurrent_forks` remains the standalone Fork fallback. Neither
   is a second EKO product quota.
7. `Sync` and `Fork` describe waiting and isolation, not separate Subagent quotas. `AgentPool`
   remains responsible for Agent instances, workspace, model, memory, and leases; it does not
   own the task graph or Subagent hierarchy.

## Trade-offs

One TaskRun preserves a consistent lifecycle for the goal, revisions, dependencies, recovery,
and evidence, at the cost of stricter revision/CAS handling for dynamic expansion. Disallowing
recursive Subagents prevents unbounded fan-out, resource exhaustion, and ambiguous result
ownership, at the cost of routing complex coordination through the primary agent and one graph.
One product concurrency quota reduces the mental split between several semaphores, while
framework standalone consumers may still retain their compatibility configuration. EKO removes
the duplicate Subagent process governor but retains separate AgentPool foreground execution and
TaskRuntime write/shell/LLM resource classes.

## Impact

- framework: continue owning Task graph, Subagent depth, and generic admission; extend the
  existing `KeyedExecutionAdmission`/execution admission interface instead of adding an EKO-
  specific semaphore;
- app-core: inject EKO defaults and capability policy, and compose TaskRuntime, AgentPool, and
  durable projections;
- generated contracts/tests/docs: synchronize depth rejection, capacity, configuration, and
  recovery behavior.

## Status and follow-up

This ADR records the confirmed target execution model and remains Proposed. Before implementation,
create an execution plan bound to this design revision for framework, app-core, and verification
deliverables. Change it to Accepted and attach commit evidence after code, test, and recovery
verification pass.
