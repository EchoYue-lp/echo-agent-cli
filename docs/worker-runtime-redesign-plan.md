# Worker Runtime Redesign Plan

This document is the durable execution plan for redesigning EchoCoWork's approval modes, run modes, worker/subagent parallelism, and GUI trace visibility.

## North Star

EchoCoWork should show one coherent run:

- The main assistant thinking and response.
- The active workers/subagents underneath the run.
- Each worker can be opened to inspect its own thinking, tools, results, errors, and artifacts.
- Chat, Task, and Auto modes use the same observable runtime event model.
- Parallel work is orchestrated by runtime code, not by hoping the LLM emits several `agent_tool` calls.

## Current Diagnosis

The existing system has useful pieces, but they are connected through uneven paths:

- Chat streaming already exposes thinking events through `chat://event`.
- Subagent lifecycle events already exist, but only expose started/completed/failed/cancelled state.
- TaskRuntime and ParallelReadonlyDelegation run useful work, but their agent execution paths are not consistently stream-observable in the GUI.
- `agent_tool` is useful as a compatibility tool, but it is the wrong primary abstraction for deterministic parallel delegation.
- Skills are real and loaded, but runtime refresh, UI enable/disable, and compression protection still need hardening.
- Permission approval and interaction mode were previously mixed; recent commits separated user-facing approval mode from planning/task runtime mode. Further cleanup remains.

## Design Principles

1. Make worker execution a first-class runtime object.
2. Treat tool calls as one possible worker action, not as the worker orchestration mechanism.
3. Use one event protocol across Chat, Task, Auto, TaskRuntime workers, and subagents.
4. Keep approval mode separate from interaction mode.
5. Keep compatibility with `agent_tool` while moving primary orchestration into runtime code.
6. Every phase must have an observable acceptance test in the GUI or logs.

## Target Concepts

### Run

A user-visible top-level execution. A chat turn, task run, or auto-routed complex job is a run.

Core fields:

- `run_id`
- `conversation_id`
- `mode`: `chat | task | auto`
- `status`: `running | completed | failed | cancelled`
- `started_at`
- `completed_at`

### Worker

A child execution unit owned by a run. Examples:

- `project_explorer`
- `code_reviewer`
- `test_planner`
- `summary_writer`
- skill-backed worker
- readonly research worker
- data analysis worker

Core fields:

- `worker_id`
- `run_id`
- `parent_worker_id`
- `agent_name`
- `title`
- `task`
- `status`
- `started_at`
- `completed_at`

### Trace Event

Append-only observable event scoped to either a run or a worker.

Core event kinds:

- `run_started`
- `run_completed`
- `run_failed`
- `worker_planned`
- `worker_started`
- `worker_thinking_start`
- `worker_thinking_delta`
- `worker_thinking_end`
- `worker_tool_start`
- `worker_tool_result`
- `worker_token_delta`
- `worker_artifact`
- `worker_completed`
- `worker_failed`
- `approval_requested`
- `approval_resolved`

## Phase 0: Baseline And Guardrails

Goal: record the current behavior and prevent accidental regressions.

Tasks:

- Capture current Chat streaming behavior for thinking/tool/result.
- Capture current TaskRuntime behavior for Auto/Task modes.
- Capture current subagent lifecycle behavior from `subagent://event`.
- Add focused tests around permission mode normalization and no-spurious-approval behavior from the recent fixes.

Primary files:

- `echo-agent-cli/src/tauri/commands/chat.rs`
- `echo-agent-cli/src/tauri/mod.rs`
- `echo-agent-cli/echo-agent-app-core/src/tasks/task_runtime/`
- `echo-agent-cli/web-frontend/src/stores/chatStore.ts`
- `echo-agent-cli/web-frontend/src/stores/subagentStore.ts`

Acceptance:

- A short manual script can reproduce:
  - Chat mode shows thinking.
  - Task/Auto currently do not fully show worker thinking.
  - Subagent cards show lifecycle only.
- Tests pass before structural changes start.

## Phase 1: Unified Trace Event Model

Goal: define the shared event vocabulary before changing orchestration.

Tasks:

- Add a canonical runtime trace event type in app-core.
- Add conversion from existing chat streaming events into trace events.
- Add conversion from existing subagent lifecycle events into trace events.
- Add frontend store support for run-scoped and worker-scoped traces.
- Keep existing `chat://event` and `subagent://event` while adding the new protocol.

Primary files:

- `echo-agent-cli/echo-agent-app-core/src/tasks/task_runtime/types.rs`
- `echo-agent-cli/src/tauri/mod.rs`
- `echo-agent-cli/src/tauri/commands/chat.rs`
- `echo-agent-cli/web-frontend/src/stores/chatStore.ts`
- `echo-agent-cli/web-frontend/src/stores/subagentStore.ts`

Implementation notes:

- Prefer additive events first.
- Do not remove legacy event payloads in this phase.
- Use stable IDs so the GUI can merge events from chat, task runtime, and subagents.

Acceptance:

- Chat mode still works.
- Subagent lifecycle cards still work.
- New trace events are visible in logs or a dev panel.

## Phase 2: Stream TaskRuntime And Auto Paths

Goal: Task and Auto modes should produce the same thinking/tool trace quality as Chat.

Tasks:

- Replace non-streaming agent execution in TaskRuntime/readonly delegation with streaming execution where possible.
- Bridge stream events into the unified trace model.
- Fix any `plan_ready` or task transition event that leaves a stale streaming message.
- Ensure cancellation flows from top-level run into workers.

Primary files:

- `echo-agent-cli/echo-agent-app-core/src/tasks/task_runtime/executor.rs`
- `echo-agent-cli/echo-agent-app-core/src/tasks/task_runtime/delegation.rs`
- `echo-agent-cli/src/tauri/commands/chat.rs`
- `echo-agent-cli/web-frontend/src/lib/chatEventHandler.ts`

Acceptance:

- Chat, Task, and Auto all show thinking.
- Starting a complex review in Auto shows worker rows while the run is active.
- Cancelling the run stops active workers.

## Phase 3: Worker Cards With Expandable Traces

Goal: implement the product shape shown in the reference screenshot.

Tasks:

- Extend `subagentStore` or replace it with a `workerRunStore`.
- Store thinking segments, tool calls, outputs, status, duration, and errors per worker.
- Add expandable worker UI under the active assistant message/run.
- Show stable worker labels, not raw implementation IDs.
- Add empty/loading/completed/error states.

Primary files:

- `echo-agent-cli/web-frontend/src/stores/subagentStore.ts`
- `echo-agent-cli/web-frontend/src/components/`
- `echo-agent-cli/web-frontend/src/lib/chatEventHandler.ts`
- `echo-agent-cli/web-frontend/src/generated/`

Acceptance:

- The main run shows a "working" duration.
- Active workers are visible while running.
- Clicking a worker shows its thinking/tool/result trace.
- Completed workers remain inspectable after final answer.

## Phase 4: Runtime-Orchestrated Parallel Delegation

Goal: move primary parallel delegation out of `agent_tool` prompt behavior and into deterministic runtime orchestration.

Tasks:

- Introduce a `WorkerPlanner` or `DelegationPlanner` that maps a complex readonly task into worker specs.
- Use domain signals flexibly, not hard-coded vertical silos. A medical research task may need literature, evidence review, data analysis, and coding workers.
- Let the runtime choose worker count based on task shape, available agents, and max concurrency.
- Call internal delegation APIs directly instead of relying on LLM-generated `agent_tool` calls.
- Keep `agent_tool` as an LLM-accessible compatibility escape hatch.

Primary files:

- `echo-agent-cli/echo-agent-app-core/src/tasks/task_runtime/classify.rs`
- `echo-agent-cli/echo-agent-app-core/src/tasks/task_runtime/delegation.rs`
- `echo-agent/src/agent/react/mod.rs`
- `echo-agent/src/agent/subagent/executor.rs`
- `echo-agent/src/tools/builtin/agent_dispatch.rs`

Planning model:

- Inputs: user request, workspace facts, available workers, allowed tools, interaction mode.
- Output: `Vec<WorkerSpec>`.
- WorkerSpec fields: `agent_name`, `title`, `task`, `purpose`, `readonly`, `expected_output`, `depends_on`.

Acceptance:

- "Analyze this project" in Auto deterministically creates multiple workers without requiring the model to call `agent_tool`.
- Worker count is not fixed to 3; it follows task complexity and worker availability.
- Worker start/completion appears in the GUI immediately.

## Phase 5: Approval And Mode Cleanup

Goal: make modes boring and understandable.

User-facing interaction modes:

- `Chat`: simple conversation and direct assistance.
- `Task`: user explicitly asks for structured execution.
- `Auto`: runtime decides whether to stay in Chat or promote to Task/Workers.

User-facing approval modes:

- `default`: high-risk operations ask.
- `auto-edit`: reads and file edits pass; execute/network/sensitive ask.
- `full-auto`: tools pass unless explicitly blocked.
- `strict`: write/execute/network/sensitive ask.

Internal-only concepts:

- readonly task planning
- worker orchestration
- planning policy
- protected paths

Tasks:

- Audit remaining `PermissionMode` variants.
- Delete or hide dead variants if they do not serve active paths.
- Keep compatibility normalization for saved config only.
- Ensure `spawn_background_task` and sandbox/shell execution have consistent approval semantics.

Primary files:

- `echo-agent/echo-core/src/tools/permission.rs`
- `echo-agent/echo-orchestration/src/human_loop/service.rs`
- `echo-agent/src/tools/builtin/spawn_task.rs`
- `echo-agent-cli/web-frontend/src/lib/permissionModes.ts`
- `echo-agent-cli/src/cli/cmd_impls/coding.rs`
- `echo-agent-cli/src/tauri/commands/panels.rs`

Acceptance:

- No frontend option maps silently to an unrelated approval mode except legacy compatibility.
- Shell/background task paths have consistent approval behavior.
- Approval prompts do not appear during planning/sorting phases.

## Phase 6: Skills Runtime Hardening

Goal: make skills feel intentionally productized, not hidden infrastructure.

Tasks:

- Wrap IntentRouter skill injection with the same `<skill_content>` protection used by `activate_skill`.
- Refresh loaded skills after install/load where possible.
- Add GUI affordances for skill search, enable, disable, unload, and load directory.
- Show active skills in run/worker trace metadata.

Primary files:

- `echo-agent/src/agent/react/run/react_loop.rs`
- `echo-agent/src/agent/react/run/stream_channel.rs`
- `echo-agent-cli/echo-agent-app-core/src/skills_hub/`
- `echo-agent-cli/src/cli/cmd_impls/skills.rs`
- `echo-agent-cli/src/tauri/commands/panels.rs`
- `echo-agent-cli/web-frontend/src/components/skills/`

Acceptance:

- Activating a skill through IntentRouter survives context compression.
- Installing/loading a skill can affect the active agent without restarting, or the UI clearly states when restart is required.
- GUI can show which skills are available and active.

## Phase 7: Cleanup, Tests, And Documentation

Goal: remove transitional duplication once the new path is stable.

Tasks:

- Remove obsolete event glue.
- Remove dead PermissionMode branches if confirmed unused.
- Add regression tests for:
  - Auto complex task creates worker specs.
  - Task mode streams thinking.
  - Worker trace receives tool events.
  - Permission prediction does not ask the user.
  - Skill activation survives compression markers.
- Update docs and screenshots.

Acceptance:

- `cargo fmt --all`
- `cargo check --workspace`
- `cargo test --workspace`
- GUI target check/test.
- Frontend `npx tsc -b`
- Frontend `npm run build`
- Manual GUI smoke test for Chat, Task, Auto, worker expansion.

## Suggested Execution Order

1. Phase 1 and Phase 2 first, because observability must exist before deeper orchestration changes.
2. Phase 3 next, because it gives the product-visible worker experience.
3. Phase 4 after the GUI can show workers, because deterministic fan-out needs visibility while debugging.
4. Phase 5 and Phase 6 after the main runtime path is visible.
5. Phase 7 last.

## Commit Strategy

Use small commits that can be reviewed and reverted:

1. `feat(runtime): add unified worker trace events`
2. `feat(task-runtime): stream task and auto worker execution`
3. `feat(gui): show expandable worker traces`
4. `feat(runtime): orchestrate readonly worker fanout`
5. `refactor(permission): simplify mode surface`
6. `fix(skills): harden activation and runtime refresh`
7. `test(runtime): cover worker trace and delegation paths`

## Resume Checklist

When resuming from context compaction, check this list first:

1. Read this document.
2. Check `git status` in both `echo-agent` and `echo-agent-cli`.
3. Identify the active phase.
4. Run the phase's acceptance checks before moving on.
5. Do not start Phase 4 until Phase 1-3 trace visibility is working.

