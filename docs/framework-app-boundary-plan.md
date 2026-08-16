# Framework / App Boundary Migration Plan

> Status: Phase 1-4 complete; app role-capability registration and
> policy-gated nested delegation are wired.
> Scope: `echo-agent` is the reusable framework; `echo-agent-cli` is the EKO app.
> Rule of thumb: extract framework kernels and traits, not EKO product state.
> 2026-07-27 postscript: the historical `ConcurrencyLimits` proposal was
> narrowed. The framework now owns only `max_concurrent_subagents`; EKO owns
> writer/shell/LLM limits in `EkoExecutionLimits`. See
> `docs/2026-07-27-runtime-dag-kernel-convergence.md`.

## Why This Exists

The current TaskRuntime implementation in `echo-agent-app-core` contains both:

- reusable agent orchestration ideas, such as DAG scheduling, subagent traits,
  concurrency limits, subagent execution summaries, and bounded follow-up task
  suggestions;
- EKO-specific product behavior, such as `events.jsonl` / `plan.json`,
  Tauri event payloads, GUI todo projections, `DomainProfile`, route policy,
  approval gates, and local worktree naming.

Moving the whole module into `echo-agent` would pollute the framework with app
decisions. Leaving all of it in app-core makes the framework miss reusable
runtime primitives. The migration should split the reusable kernel from the EKO
adapter.

## Reference Implementations

- Claude Code: subagents have isolated context and explicit tool permissions;
  nested subagents are supported only when the `Agent` tool is available and
  depth is bounded. Takeaway: nested delegation is a capability gate, not a
  default right of every subagent.
- Codex: subagents are mainly parallel subagents. The main thread owns task
  decomposition, waits for subagents, and synthesizes results. Takeaway: keep one
  authoritative planner / integrator.
- Cursor: public material emphasizes product-level multiple agents / background
  agents, not arbitrary recursive task-runtime ownership. Takeaway: parallelism
  can be surfaced at product level without making every subagent a new planner.

EKO adopts a hybrid:

1. Default subagents execute the current `PlanTask`.
2. Subagents may return structured `suggested_tasks`.
3. Optional nested delegation is enabled per role with an explicit capability
   bit and a depth limit.

Implementation note: nested delegation depth has one authority:
`echo_agent::tasks::NestedDelegationPolicy` (re-exported from
`echo_core::tools`). The policy flows through `ExternalRunContext` into
`ToolContext`, and `agent_tool` derives a child policy before dispatching a
nested subagent. The older subagent executor `delegate_depth` integer was
replaced in `DispatchRequest` by this policy; old helper APIs may still accept a
depth parameter, but they immediately convert it to `NestedDelegationPolicy`.

## Hard Boundaries

### Must Stay In `echo-agent-cli`

These are EKO product concerns:

- `TaskRuntimeStore` file authority and its concrete layout:
  `events.jsonl`, `plan.json`, append/rebuild semantics, `FileTaskShadow`.
- GUI/Tauri event protocol:
  `execution://event`, `chat://event`, `task_id` payload shape, RightRail todo
  projection, frontend polling policy.
- EKO run routing and policy:
  `TaskRouteKind`, `DomainProfile`, `ExecutionPolicySnapshot`,
  `ComplexRuntime`, attended/unattended gates.
- EKO persistence and memory bridge:
  `memory_bridge.rs`, app memory candidate policy, conversation binding,
  root message id conventions.
- Local desktop app worktree details:
  branch naming, directory layout, data workspace cleanup, EKO-specific
  worktree factory behavior.
- Product tools:
  `create_complex_task`, `task_create/update/complete/skip/list`,
  `execute_plan` in its current EKO form, `cancel_run`, `check_run_status`.

### Good Candidates For `echo-agent`

These are reusable framework capabilities:

- Task graph primitives:
  `TaskId`, `TaskStatus`, dependency edges, terminal-state helpers.
- DAG executor kernel:
  frontier computation, dependency satisfaction, in-flight detection, wave
  dispatch, join aggregation, cancellation propagation.
- Subagent abstraction:
  a generic `TaskSubagent` trait that receives a task and returns a typed
  `TaskExecutionSummary`.
- Runtime concurrency model:
  `ConcurrencyLimits`, subagent/write/shell/LLM semaphores as generic knobs.
- Follow-up suggestions:
  `SuggestedTask` and structured subagent summaries, with app-owned append
  policy.
- Nested delegation policy:
  `can_spawn_subagents`, `delegate_depth`, `max_delegate_depth`, and allowed
  delegate tool configuration.
- Event-neutral trace types:
  generic run/task lifecycle events that EKO can map to Tauri payloads.

### Candidates To Move From `echo-agent` To `echo-agent-cli`

Audit these before moving. Do not delete framework APIs merely because EKO does
not use them.

- `spawn_task` / `check_task` builtin tools:
  If they model a generic framework background-task API, keep the traits in the
  framework. If they encode a specific app UX or task store, move the concrete
  tools to CLI.
- Task relation tools:
  The framework owns `task_create/task_update/task_list`, revision/patch
  semantics, validation, and the default in-memory store. EKO supplies only
  its file store and product-policy adapters.
- `AgentDispatchTool`:
  Keep the generic delegate tool in framework. CLI decides whether a given
  subagent role receives it.

## Target Architecture

```text
echo-agent
  task_runtime/
    types.rs          # generic task graph + summaries + suggested tasks
    executor.rs       # generic DAG executor kernel
    subagent.rs         # TaskSubagent trait
    policy.rs         # generic nested delegation policy
    events.rs         # generic runtime lifecycle events

echo-agent-cli / echo-agent-app-core
  tasks/task_runtime/
    types.rs          # EKO-specific run/plan/todo envelopes, or adapters
    store.rs          # file-backed authority
    task_execute_tool.rs
    executor_adapter.rs
    event_rebuild.rs
    file_shadow.rs
    router.rs
    policy.rs         # EKO route policy, not framework policy
```

The framework executor must not know about Tauri, JSONL file paths, GUI todos,
or EKO route names. The CLI adapter translates framework events and summaries
into EKO storage and UI projections.

## Phase Plan

### Phase 1: Extract Type Kernel

Status: complete.

Commits:

- `701f1f6` (`echo-agent`): generic runtime primitives.
- `a530875` (`echo-agent-cli`): EKO-to-framework conversions.
- `00acb40` (`echo-agent-cli`): reuse framework concurrency limits.
- `ef8f02d` (`echo-agent`): `TaskSubagentContext`.

Goal: add framework types without changing runtime behavior.

Move or duplicate-then-adapt into `echo-agent`:

- `SuggestedTask`
- a generic `TaskExecutionSummary`
- `ConcurrencyLimits`
- `TaskSubagent` trait
- terminal-status helpers
- nested delegation policy structs

Keep CLI using existing app-core types initially. Add explicit conversion
functions from EKO types to framework types.

Verification:

```bash
cd echo-agent
./scripts/verify-all-crates.sh --quick

cd ../echo-agent-cli
cargo check --workspace
npx --prefix web-frontend tsc -b
```

### Phase 2: Extract DAG Scheduling Kernel

Status: complete.

Commits:

- `d3f7183` (`echo-agent`): DAG runtime kernel.

Goal: move pure graph scheduling to framework, still driven by CLI store.

Framework owns:

- ready frontier calculation
- dependency completion checks
- in-flight skip logic
- wave dispatch and join
- cancellation outcome aggregation

CLI still owns:

- loading plans
- writing task status
- persisting summaries
- review gates
- file/worktree locks
- memory bridge
- Tauri events

Implementation shape:

```rust
pub trait TaskRuntimeStoreAdapter {
    type Task;
    type Summary;

    fn task_id(task: &Self::Task) -> &str;
    fn dependencies(task: &Self::Task) -> &[String];
    fn is_terminal(task: &Self::Task) -> bool;
    fn is_running(task: &Self::Task) -> bool;
}
```

The exact trait can change, but the direction is: framework asks generic
questions; CLI performs app-specific reads/writes.

### Phase 3: Rewire EKO Executor To Framework Kernel

Status: mostly complete.

Commits:

- `2c74839` (`echo-agent-cli`): app runtime uses framework DAG kernel.
- `4c33fa9` (`echo-agent-cli`): dispatcher naming cleanup.
- `fb44e35` (`echo-agent-cli`): dispatcher receives framework
  `TaskSubagentContext`.

Goal: app-core `executor.rs` becomes an adapter around framework runtime.

Expected app-core responsibilities after this phase:

- convert `PlanTask` to framework task view;
- pass a `TaskSubagent` implementation that invokes EKO subagents;
- handle review gate and `SuggestedTask` append policy;
- map generic lifecycle events to `ExecEvent` / `execution://event`;
- persist status through `TaskRuntimeStore`.

Delete duplicated scheduling code from app-core only after the adapter is green.

### Phase 4: Tool Boundary Cleanup

Status: audit complete; no tool move needed yet.

Findings:

- The old `spawn_background_task` / `check_task_status` / `list_background_tasks`
  trio was replaced by the generic framework command-cell surface: `shell
  background=true` + `wait` / `stop_cell` / `list_cells` (`echo-core`
  `CommandCellRegistry` contract, `echo-orchestration`
  `BackgroundCommandManager`), injected via `ReactAgentBuilder::command_cells`.
- Process-global `todo_write` was removed and replaced by the framework's
  instance-local revisioned `task_create/task_update/task_list` tools.
- `agent_tool` remains the generic framework delegate tool. EKO decides whether
  a role receives it via capability / registration policy.
- EKO-specific run tools (`create_complex_task`, `task_execute`, `cancel_run`,
  `check_run_status`) stay in `echo-agent-cli`; task CRUD tools live in the
  framework.
- EKO subagent `.md` frontmatter supports `can_delegate: true`. Default
  subagents do not receive `agent_tool`; only explicitly marked roles get it.
  Delegate-capable roles receive every non-self child subagent, including other
  delegate-capable roles. Recursion is bounded by `NestedDelegationPolicy`
  flowing through `ExternalRunContext` -> `ToolContext` -> `agent_tool`.

Commit:

- `2b2c958` (`echo-agent`): subagent dispatch uses
  `NestedDelegationPolicy` instead of a separate `delegate_depth` field.
- `echo-agent-cli`: role-capability registration for nested delegation via
  `can_delegate`.
- `echo-agent`: `NestedDelegationPolicy` is now available in framework
  `ToolContext`.
- `echo-agent-cli`: delegate-capable child registry is open to all non-self
  roles and remains policy-gated by max depth.

Goal: make tool placement match product/framework boundary.

Actions:

1. Audit framework builtin tools.
2. Keep generic tools in framework.
3. Move app-specific concrete tools to CLI.
4. Leave framework traits / registries intact when they are valid public API.
5. Update registration so main agent and subagents receive tools by capability:
   default subagent, suggested-task subagent, and optional nested-delegate subagent.

## Non-Goals

- Do not move EKO file authority into framework.
- Do not introduce SQLite to CLI.
- Do not add a new run-state explosion for planning/approval.
- Do not make every subagent capable of recursive delegation.
- Do not delete framework public APIs just because CLI does not call them.

## Acceptance Criteria

- `echo-agent` exposes reusable task runtime primitives without depending on
  `echo-agent-cli`.
- `echo-agent-cli` behavior remains unchanged for user task -> plan -> parallel
  subagent execution.
- Subagent `suggested_tasks` still append only through EKO TaskRuntime.
- GUI task list still joins subagents by `task_id`.
- Both TUI and GUI retain feature parity.
- Verification passes in both repositories before any migration commit:

```bash
cd echo-agent
./scripts/verify-all-crates.sh
cargo clean

cd ../echo-agent-cli
cargo fmt --all
cargo fmt --all -- --check
cargo check --workspace
cargo test --workspace
cargo check --no-default-features --features gui --bin echo-agent-tauri
cargo test --no-default-features --features gui
cd web-frontend
npx tsc -b
npm run build
cd ..
cargo clean
```
