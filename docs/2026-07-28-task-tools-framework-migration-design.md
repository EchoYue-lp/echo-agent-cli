# Task Tools Framework Migration — Design

> Status: **Proposed** (awaiting user review)
> Date: 2026-07-28
> Author: agent session
> Related: `docs/framework-app-boundary-plan.md` (Phase 1–4 complete, 2026-07-27);
> `docs/MASTER-PLAN.md` "Runtime DAG kernel convergence"
> Scope: `echo-agent` (framework) + `echo-agent-cli/echo-agent-app-core` (EKO app)

---

## 1. Background and Why This Is Narrow

### 1.1 The prior boundary migration is done

A full framework/app boundary migration was already executed
(`docs/framework-app-boundary-plan.md`, Phase 1–4). As of 2026-07-27 the
**Runtime DAG kernel** lives in the framework:

- `echo-orchestration/src/tasks/runtime.rs` — `Task`/`TaskSpec`/`TaskExecution`/
  `TaskStatus`/`TaskClaim`/`TaskKind`/`TaskSubagent` trait
- `echo-orchestration/src/tasks/runtime_executor.rs` — `RuntimeDagExecutor` +
  `RuntimeDagController` trait (the application adapter seam)
- `echo-orchestration/src/planning/` — `PlanSpec`/`PlanValidator`
- `echo-agent/src/agent/subagent/` — registry, executor, prompt compiler trait

EKO's `echo-agent-app-core/src/tasks/task_runtime/` is now a **correct adapter**:
`EkoRuntimeDagController` implements `RuntimeDagController`; `TaskRuntimeStore`
holds file-backed persistence + product recovery/audit; `EkoRuntimeDagController`
owns review/worktree/write-mode policy.

**That work is not in scope here.** The DAG kernel, validator, claim protocol,
and dispatch loop are settled. Do not reopen.

### 1.2 The one real gap

The framework still ships the **deprecated `todo_write`** scratchpad
(`echo-agent/src/tools/builtin/todo.rs`), while Claude Code (the design
reference cited throughout this codebase) **deprecated `TodoWrite` in favor of
`TaskCreate`/`TaskUpdate`/`TaskList`/`TaskGet`**. EKO already implements the
modern form in the app layer (`task_tools.rs`), then removes `todo_write` at
registration time (`register.rs:38`).

This is a genuine inversion: the modern, industry-recognized task-tool surface
is trapped in the product layer, and the framework is stuck on a form the
reference product has abandoned. **This design closes that gap** by migrating
`task_create` / `task_update` / `task_list` into the framework behind a
product-neutral trait, and removing the deprecated `todo_write`.

### 1.3 What stays in the app

Per the deep-dive decomposition (see Appendix A), the four other tools are
genuinely product-bound and **do not migrate**:

| Tool | Why it stays |
|---|---|
| `task_execute` | Calls `execute_run` (EKO DAG executor), unattended preflight, `AgentHandle`/reviewer LLM wiring, `register_run_cancellation` (Arc-receiver, in-memory). ~70% product logic. |
| `create_complex_task` | Spin up formal TaskRun + dispatch via `run_driver::drive_run_async`; reads `current_chat_resources()`; binds `DomainProfile`/`RunPlanPolicy`. Pure EKO orchestration launch. |
| `check_run_status` | Reads `current_chat_resources()` task_local; thin enough to neutralize later if a use case appears, but no value today. |
| `cancel_run` | Calls `request_cancel`, which mixes in-memory driver tokens with persisted run-state transitions (product concern). |

These stay in `echo-agent-app-core/src/tasks/task_runtime/`.

---

## 2. Reference Implementations (per AGENTS.md "调研业界")

- **Claude Code** ([Todo Lists — Agent SDK docs](https://code.claude.com/docs/en/agent-sdk/todo-tracking)):
  `TodoWrite` deprecated; replaced by `TaskCreate` (per item), `TaskUpdate`
  (per status change), `TaskList`, `TaskGet`. Includes dependency/blocker
  tracking. **Strong signal**: the migrated surface mirrors this exactly.
- **Codex** ([openai/codex#24547](https://github.com/openai/codex/issues/24547)):
  proposal for task/plan lifecycle hooks + external plan-update API. Same
  direction — revisioned plan edits exposed to the agent as discrete tools.
- **EKO's existing implementation** is itself prior art: it already follows the
  Claude Code model. Migration is "lift the neutral core, leave the product
  adapter" — not greenfield design.

**Takeaway**: the tool *shape* (per-item create, revisioned patch update, list)
is industry-consensus. The *persistence* (file vs sqlite vs in-memory) and the
*run bootstrap policy* are product choices and stay behind a trait.

---

## 3. Target Architecture

```
echo-agent  (framework)
  echo-orchestration/src/tasks/
    runtime.rs            # existing: Task, TaskSpec, TaskStatus, TaskKind, TaskClaim
    revisioned_store.rs   # NEW: RevisionedTaskStore trait + neutral DTOs
    task_tools/           # NEW module
      mod.rs              #   TaskCreateTool, TaskUpdateTool, TaskListTool
      schema.rs           #   JSON schemas (product-neutral task shape)
  src/tools/builtin/
    todo.rs               # REMOVED (deprecated scratchpad)

echo-agent-cli / echo-agent-app-core  (EKO app)
  tasks/task_runtime/
    types.rs              # EKO DTOs (EkoTaskSpec, EkoTaskExecution, PlanTask...)
                          #   REMAIN — they are file/UI projections, not authority
    store.rs              # TaskRuntimeStore: impl RevisionedTaskStore (NEW impl block)
    task_tools.rs         # SLIMMED — only CreateComplexTask/CheckRunStatus/
                          #   CancelRun stay; TaskCapabilityCatalog stays (product)
    task_execute_tool.rs  # UNCHANGED
    register.rs           # UPDATED — no longer removes todo_write (framework
                          #   doesn't ship it); still adds EKO-specific tools
```

### 3.1 Layering rule (enforced)

- **Framework owns**: tool name, parameter schema, revisioned-patch wire
  protocol, neutral DTOs, optimistic-concurrency validation, summary formatting,
  and the trait that persistence must implement.
- **App owns**: persistence backend (file/sqlite/in-memory), run bootstrap
  policy, capability catalog validation (subagent roles, tool allowlist),
  domain-profile defaulting, all execution triggering (`task_execute` and below).

The framework tool calls **only** the trait; the trait is implemented in the
app. No `TaskRuntimeStore` symbol crosses into `echo-agent`.

---

## 4. The `RevisionedTaskStore` Trait

New trait in `echo-orchestration/src/tasks/revisioned_store.rs`. It sits
**between** the existing flat `TaskStore` (too thin — no claim/revision/run) and
the heavy `RuntimeDagController` (has `type DispatchOutput`, not dyn-safe, and
owns dispatch which task_create/update/list do not need).

### 4.1 Required surface (derived from actual tool call sites — see Appendix B)

```rust
// echo-orchestration/src/tasks/revisioned_store.rs

use echo_core::error::Result;
use super::runtime::{Task, TaskId, TaskSpec};

/// A coherent revisioned snapshot of one task graph (one TaskRun).
#[derive(Debug, Clone)]
pub struct RevisionedPlan {
    pub revision: u64,
    pub tasks: Vec<Task>,
}

/// Optimistic-concurrency patch applied atomically to one revision.
#[derive(Debug, Clone)]
pub struct PlanPatch {
    pub base_revision: u64,
    pub reason: String,
    pub operations: Vec<PlanPatchOp>,
}

#[derive(Debug, Clone)]
pub enum PlanPatchOp {
    Insert { after_task_id: Option<TaskId>, spec: TaskSpec },
    Update { task_id: TaskId, patch: TaskSpecPatch },
    Skip   { task_id: TaskId },
    Reorder{ task_ids: Vec<TaskId> },
}

/// Partial update of one TaskSpec. Mirrors EKO's `TaskPatch`
/// (types.rs:1085) but with framework-neutral field types (no DomainProfile).
#[derive(Debug, Clone, Default)]
pub struct TaskSpecPatch {
    pub title:               Option<String>,
    pub description:         Option<String>,
    pub kind:                Option<TaskKind>,
    pub agent_role:          Option<String>,
    pub depends_on:          Option<Vec<TaskId>>,
    pub files:               Option<Vec<String>>,
    pub allowed_tools:       Option<Vec<String>>,
    pub required_artifacts:  Option<Vec<String>>,
    pub execution_checks:    Option<Vec<String>>,
    pub acceptance_criteria: Option<Vec<String>>,
    pub max_retries:         Option<u32>,
    pub metadata:            Option<serde_json::Value>, // product extension bag
}

/// Conflict returned when `base_revision` is stale.
#[derive(Debug, Clone, thiserror::Error)]
#[error("plan revision conflict: expected {expected}, current {current}")]
pub struct PlanRevisionConflict {
    pub expected: u64,
    pub current: u64,
}

/// Persistence + run-bootstrap seam for the migrated task tools.
///
/// Implementations are product-supplied. The framework tools call ONLY this
/// trait; no concrete store type crosses the boundary.
///
/// All methods are `Send + Sync` and object-safe (no `Self` types, no
/// `&Arc<Self>` receivers, no generic methods).
#[async_trait::async_trait]
pub trait RevisionedTaskStore: Send + Sync {
    /// Read the current revisioned plan for `run_id`.
    /// Returns `Ok(None)` when no plan exists yet (caller may bootstrap).
    async fn load_plan(&self, run_id: &str) -> Result<Option<RevisionedPlan>>;

    /// Atomically apply `patch` to the plan for `run_id`.
    /// MUST reject with `PlanRevisionConflict` when `base_revision` is stale.
    /// MUST validate dependencies / cycles via the framework `PlanValidator`
    /// before commit (or delegate to an injected validator).
    async fn apply_patch(
        &self,
        run_id: &str,
        patch: PlanPatch,
    ) -> std::result::Result<RevisionedPlan, PlanRevisionConflict>;

    /// Ensure a task graph (TaskRun) exists for `run_id`.
    ///
    /// If one already exists, return `Ok(())` unchanged.
    /// If not, the implementation creates one using product policy
    /// (EKO: derive conversation/message/attachments from `ToolContext`,
    /// pick a default DomainProfile, transition to Running, emit ExecEvent).
    ///
    /// `bootstrap_ctx` carries the framework `ToolContext` so the app can read
    /// `conversation_id`, `message_id`, `attachments` without a task_local.
    async fn ensure_run_exists(
        &self,
        run_id: &str,
        goal: &str,
        bootstrap_ctx: Option<&echo_core::tools::ToolContext>,
    ) -> Result<()>;

    /// List tasks for `run_id` as neutral `Task` views (UI/list tool).
    async fn list_tasks(&self, run_id: &str) -> Result<Vec<Task>>;
}
```

### 4.2 Why this shape

- **Object-safe** (`dyn RevisionedTaskStore` works): no `Self` types, no
  `&Arc<Self>`, no generic methods, no `impl Trait` returns. The current
  `RuntimeDagController` fails this (`type DispatchOutput`), `TaskStore` is
  object-safe but too flat.
- **Carries revision semantics**: `apply_patch` is the optimistic-concurrency
  primitive Claude Code and EKO both use; `PlanRevisionConflict` is the typed
  error tools translate into a user-facing message.
- **Run bootstrap is a hook, not a behavior**: `ensure_run_exists` lets the app
  inject its product policy (EKO's "every task_create in Auto mode materializes
  a formal run") without the framework knowing about DomainProfile / route /
  AttendedMode. The framework passes the `ToolContext`; the app reads what it
  needs.
- **Neutral DTOs**: `TaskSpec` / `TaskKind` already live in the framework
  (`runtime.rs`). `TaskSpecPatch` mirrors EKO's `TaskPatch` minus
  DomainProfile-specific fields, plus a `metadata: serde_json::Value` extension
  bag so the app can round-trip product fields (parallel_group, sort_order,
  domain_profile) without polluting the framework type.

### 4.3 Where it lives

`echo-orchestration/src/tasks/revisioned_store.rs`, gated behind the existing
`tasks` feature (already on for EKO). Re-exported via the `echo_agent::tasks`
facade (`src/tasks.rs`).

---

## 5. The Migrated Tools

Three new tools in `echo-orchestration/src/tasks/task_tools/` (new module).
Each mirrors the `AgentDispatchTool` pattern (`agent_dispatch.rs:57-98`):
constructed with `Arc<dyn RevisionedTaskStore>`, override
`execute_with_context`, read `ctx.run_id` for run-scoping.

### 5.1 `TaskCreateTool` (framework)

```rust
pub struct TaskCreateTool {
    store: Arc<dyn RevisionedTaskStore>,
}
```

**Behavior** (neutral core from EKO `task_tools.rs:624-766`):

1. Resolve `run_id` from `ctx.run_id` (fallback: error — no task_local).
2. Parse `task` / `tasks` (exactly one required).
3. Parse each task into a framework `TaskSpec` (id/title/description/kind/
   depends_on/agent_role/files/allowed_tools/required_artifacts/
   execution_checks/acceptance_criteria/max_retries; product fields go in
   `metadata`).
4. `store.ensure_run_exists(run_id, goal, Some(ctx))` — product hook.
5. `store.load_plan(run_id)`:
   - existing plan → build `PlanPatch { base_revision, ops: [Insert...] }`,
     `store.apply_patch(run_id, patch)`.
   - no plan → build patch with `base_revision = 0` semantics (app treats
     `base_revision == 0 && no existing plan` as "create initial"; the app's
     `attach_plan` becomes an impl detail of `apply_patch`).
6. Return summary: `"Created task graph revision N with M task(s)"`.

**What is NOT in the framework tool**: `TaskCapabilityCatalog` validation
(subagent roles, tool allowlist) — that's product policy. It stays in the app
and is invoked from the app's `apply_patch` implementation (the store trait
implementation can call into the catalog before committing).

### 5.2 `TaskUpdateTool` (framework)

```rust
pub struct TaskUpdateTool {
    store: Arc<dyn RevisionedTaskStore>,
}
```

**Behavior** (neutral core from EKO `task_tools.rs:943-1004`):

1. Resolve `run_id` from `ctx.run_id`.
2. Parse `base_revision` (required, ≥ 1), `reason` (required), `operations[]`.
3. Parse each op into `PlanPatchOp` (Insert/Update/Skip/Reorder) using neutral
   `TaskSpec` / `TaskSpecPatch`.
4. `store.apply_patch(run_id, PlanPatch { base_revision, reason, operations })`.
   - `PlanRevisionConflict` → return error `"plan revision conflict: reload
     with task_list and retry"`.
5. Return summary.

### 5.3 `TaskListTool` (framework)

```rust
pub struct TaskListTool {
    store: Arc<dyn RevisionedTaskStore>,
}
```

**Behavior** (from EKO `task_tools.rs:1878-1906`):

1. Resolve `run_id` from `ctx.run_id`.
2. `store.list_tasks(run_id)` → `Vec<Task>`.
3. Format: `"Task graph revision N — Tasks (M):\n[{status}] {id} — {title}"`.
4. Empty → `"No tasks; call task_create first"`.

### 5.4 JSON schemas (product-neutral)

The `plan_task_input_schema()` currently in EKO
(`task_tools.rs:465-486`) bakes in EKO's 8-value `kind` enum and EKO field
names. The migrated schema lives in `task_tools/schema.rs` and uses:

- `kind`: **string** (not enum) — the framework accepts any string; the app's
  `apply_patch` impl validates against its known kinds. (Forward-compatible:
  new domains don't need a framework change.)
- Standard fields: `id`, `title`, `description`, `depends_on`, `agent_role`,
  `files`, `allowed_tools`, `required_artifacts`, `execution_checks`,
  `acceptance_criteria`, `max_retries`.
- `metadata`: open object for product extension (EKO uses it for
  `parallel_group`, `sort_order`, `domain_profile`).

### 5.5 Registration — two supported paths

The framework supports **both** registration styles so different consumers can
adopt whichever fits their bootstrap order:

**Path A — inline at agent build** (for consumers whose store exists before the
agent): mirror the `SpawnBackgroundTaskTool` pattern (`react/mod.rs:421-423`).
Add a `RevisionedTaskStore` slot to the agent builder/config; when present,
`react/mod.rs` constructs and registers the three tools inline. This is the
default path for non-EKO consumers and for the framework's own integration tests.

**Path B — post-hoc registration** (for consumers like EKO whose store is built
*after* the agent): the framework exposes a free function
`echo_agent::tasks::task_tools::register_task_tools(
    agent: &mut ToolRegistrar,
    store: Arc<dyn RevisionedTaskStore>,
)` that adds the three tools to an already-built agent. EKO's
`register_task_tools_on_agent` (`register.rs:29`) calls this in place of its
current inline construction, dropping only `remove_tool("todo_write")`.

The tools themselves are identical in both paths — only the registration call
site differs.

For consumers that supply no `RevisionedTaskStore` at all, the tools are simply
absent (same as `SpawnBackgroundTaskTool` being feature-gated). The deprecated
`todo_write` is **removed** — it's been superseded industry-wide and EKO
already removes it at registration.

A minimal **in-memory default impl** (`InMemoryRevisionedTaskStore`) ships in
the framework for tests and simple consumers — same spirit as `InMemoryStore`.

---

## 6. App-Side Changes (echo-agent-app-core)

### 6.1 `TaskRuntimeStore` implements `RevisionedTaskStore`

New `#[async_trait]` impl block in
`echo-agent-app-core/src/tasks/task_runtime/store.rs`:

```rust
#[async_trait::async_trait]
impl echo_agent::tasks::RevisionedTaskStore for TaskRuntimeStore {
    async fn load_plan(&self, run_id: &str) -> Result<Option<RevisionedPlan>> {
        // existing get_plan() → map TaskPlan → RevisionedPlan (drop EKO fields)
    }
    async fn apply_patch(&self, run_id: &str, patch: PlanPatch)
        -> std::result::Result<RevisionedPlan, PlanRevisionConflict>
    {
        // 1. Validate via existing TaskCapabilityCatalog (product policy)
        // 2. Map PlanPatch → existing TaskUpdateRequest (already 1:1 shape)
        // 3. Call existing update_tasks() / attach_plan()
        // 4. Map PlanConflict → PlanRevisionConflict
        // 5. Return RevisionedPlan
    }
    async fn ensure_run_exists(&self, run_id, goal, bootstrap_ctx) -> Result<()> {
        // existing ensure_run_exists() body — reads chat_resources,
        // create_run, set_run_attachments, transition_run, ExecEvent
    }
    async fn list_tasks(&self, run_id) -> Result<Vec<Task>> {
        // existing list_todos() → map TodoItem → framework Task
    }
}
```

The mapping is **lossless in both directions**: `TaskSpec.metadata` carries
`parallel_group`/`sort_order`/`domain_profile`, and the app round-trips them.

### 6.2 Slim down `task_tools.rs`

After migration, EKO's `task_tools.rs` keeps only:
- `TaskCapabilityCatalog` (product validation — called from `apply_patch` impl)
- `CreateComplexTaskTool`, `CheckRunStatusTool`, `CancelRunTool` (product tools)
- The `parse_plan_task` helper (used by `CreateComplexTaskTool`)

The migrated `TaskCreateTool`/`TaskUpdateTool`/`TaskListTool` structs and their
~600 lines move to the framework. App's `task_tools.rs` shrinks substantially.

### 6.3 Update `register.rs` (EKO uses Path B)

- Remove `agent.remove_tool("todo_write")` (framework no longer ships it).
- Remove construction of `TaskCreateTool`/`TaskUpdateTool`/`TaskListTool` from
  app code — replace with a single call to
  `echo_agent::tasks::task_tools::register_task_tools(agent,
   store.clone() as Arc<dyn RevisionedTaskStore>)` (framework's free function,
  Path B from §5.5). EKO's bootstrap builds the agent before the store exists,
  which is why post-hoc registration is the right path here.
- Keep registration of `CreateComplexTaskTool`/`CheckRunStatusTool`/`CancelRun`/
  `task_execute` (these are product tools that stay in the app).
- Update the test `registration_replaces_framework_todo_with_one_task_api`
  (`register.rs:112`): rename and adjust assertions — `todo_write` is no longer
  in the "before" set (framework doesn't ship it), and the three migrated tools
  are now added via the framework function rather than inline `add_tool`.

### 6.4 Type cleanup (the 5 residual mirrors — optional, low-risk)

While we're touching this code, collapse the 1:1 mirror types identified in
the audit:

| App type | Action |
|---|---|
| `PlanTaskKind` (`types.rs:260`) | Delete; use framework `TaskKind` |
| `SuggestedTask` (`types.rs:1631`) | Delete; use framework `SuggestedTask` |
| `TaskExecutionSummary` (`types.rs:1573`) | Collapse onto framework type + `metadata` extension |
| `TodoStatus` (`types.rs:367`) | Keep as UI-rendering helper ONLY; ensure no scheduling code reads it |
| `EkoTaskSpec`/`EkoTaskExecution` | Keep — file DTOs, correctly stay |

This is type hygiene; can be a follow-up commit if it bloats the PR.

---

## 7. Phased Migration Plan

Each phase is independently committable and verifiable. **No phase leaves the
build broken.** Cross-repo ordering: echo-agent first (framework adds API),
then echo-agent-cli (app adopts).

### Phase 1 — Framework: add the trait + neutral DTOs (echo-agent)

**Goal**: add `RevisionedTaskStore` trait + `RevisionedPlan`/`PlanPatch`/
`PlanPatchOp`/`TaskSpecPatch`/`PlanRevisionConflict` types + an
`InMemoryRevisionedTaskStore` default impl. **No tools yet, no behavior
change.**

Files:
- `echo-orchestration/src/tasks/revisioned_store.rs` (new)
- `echo-orchestration/src/tasks/mod.rs` — `pub mod revisioned_store;`
- `src/tasks.rs` facade — re-export

Verify:
```bash
cd echo-agent
cargo fmt --all && cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-targets --all-features --locked
```

Commit: `echo-agent: add RevisionedTaskStore trait + neutral plan-patch DTOs`

### Phase 2 — Framework: add the three tools, remove `todo_write` (echo-agent)

**Goal**: `TaskCreateTool`/`TaskUpdateTool`/`TaskListTool` in
`echo-orchestration/src/tasks/task_tools/`; JSON schemas in `schema.rs`;
register them inline in `react/mod.rs` when a `RevisionedTaskStore` is present;
**delete** `src/tools/builtin/todo.rs` and its registration at `react/mod.rs:372`.

Files:
- `echo-orchestration/src/tasks/task_tools/{mod.rs,schema.rs}` (new)
- `echo-orchestration/src/tasks/mod.rs` — `pub mod task_tools;`
- `src/agent/react/mod.rs` — replace `todo_write` registration with the new
  tools, conditional on store presence
- `src/tools/builtin/mod.rs` — remove `todo` module
- `src/tools/builtin/todo.rs` — **delete**
- Tests: port EKO's `task_tools` unit tests to the framework (they test
  schema parsing, optimistic-conflict error messages, summary formatting).

Verify: same matrix as Phase 1.

Commit: `echo-agent: migrate task_create/update/list tools, remove deprecated todo_write`

### Phase 3 — App: implement `RevisionedTaskStore` for `TaskRuntimeStore` (echo-agent-cli)

**Goal**: app's `TaskRuntimeStore` gains an `impl RevisionedTaskStore` block;
the mapping is round-trip-tested.

Files:
- `echo-agent-app-core/src/tasks/task_runtime/store.rs` — add impl block
- `echo-agent-app-core/src/tasks/task_runtime/types.rs` — add
  `From<TaskSpec> for PlanTask` / `TryFrom<&PlanTask> for TaskSpec` if not
  already present (most exist — `to_task()`/`TryFrom<Task>` at types.rs:981-1065)
- Tests: round-trip tests for every field; conflict-mapping test
  (`PlanConflict` → `PlanRevisionConflict`).

Verify:
```bash
cd echo-agent-cli
cargo fmt --all && cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
```

Commit: `echo-agent-cli: impl RevisionedTaskStore on TaskRuntimeStore`

### Phase 4 — App: slim `task_tools.rs`, update `register.rs` (echo-agent-cli)

**Goal**: remove the three migrated tool structs from app code; remove
`remove_tool("todo_write")` from `register.rs`; ensure the framework tools
receive the store via the agent builder.

Files:
- `echo-agent-app-core/src/tasks/task_runtime/task_tools.rs` — remove
  `TaskCreateTool`/`TaskUpdateTool`/`TaskListTool` structs + their impls +
  `plan_task_input_schema` (now in framework). Keep `TaskCapabilityCatalog`,
  `CreateComplexTaskTool`, `CheckRunStatusTool`, `CancelRunTool`,
  `parse_plan_task` (used by CreateComplexTaskTool).
- `echo-agent-app-core/src/tasks/task_runtime/register.rs` — drop
  `remove_tool("todo_write")`; drop construction of the three migrated tools;
  pass `store.clone() as Arc<dyn RevisionedTaskStore>` into the agent builder
  path (likely `crate::runtime` / `agent_pool::from_runtime`).
- `echo-agent-app-core/src/infra.rs:489` — drop the other `remove_tool(
  "todo_write")` call.
- Update `register.rs` test (`registration_replaces_framework_todo...`) —
  renamed/assertions adjusted (framework no longer ships `todo_write`, so the
  "before" assertion changes).

Verify: full app matrix incl. GUI feature + web-frontend.

Commit: `echo-agent-cli: adopt framework task tools, slim app task_tools.rs`

### Phase 5 — (Optional) type cleanup

Collapse `PlanTaskKind`/`SuggestedTask`/`TaskExecutionSummary` mirror types.
Separate PR. Low risk, type hygiene only.

---

## 8. Risks and Mitigations

| Risk | Mitigation |
|---|---|
| `RevisionedTaskStore` not object-safe due to oversight | Audit against `echo-core/src/tools/mod.rs` `Tool` pattern; no `Self`/`&Arc<Self>`/generic methods. Add `dyn RevisionedTaskStore` test in Phase 1. |
| `TaskSpec.metadata` round-trip loses EKO fields | Round-trip test in Phase 3 covers every field; `metadata` is `serde_json::Value` so structurally lossless. |
| Removing `todo_write` breaks non-EKO consumers | Grep confirms only EKO consumes `echo-agent` today; the migration plan in `framework-app-boundary-plan.md` explicitly allows removing superseded framework APIs (Phase 4 finding: "todo_write remains a generic scratchpad for other consumers" — but there are no other consumers, and the industry has moved on). If a future consumer needs a scratchpad, they get the better `task_*` surface. |
| `ensure_run_exists` product hook needs `ToolContext` fields the framework doesn't expose | `ToolContext` already carries `conversation_id`/`message_id`/`run_id`/`turn_id`/`cancel`/`trace_sink` (`echo-core/src/tools/mod.rs:990-1018`). EKO's current `ensure_run_exists` reads exactly these. The `attachments` field is the only gap — add it to `ToolContext` if needed (small framework addition). |
| Regression in task_create/update/list behavior | Port EKO's existing unit tests verbatim to the framework in Phase 2; run EKO's integration tests in Phase 4. Behavior must be identical (same schema, same error messages, same revision semantics). |
| Two `RevisionedTaskStore` impls diverge | Only one impl exists today (`TaskRuntimeStore`). The framework `InMemoryRevisionedTaskStore` is for tests only. If a future sqlite impl appears, it implements the same trait — divergence is a property of the impl, not the design. |

---

## 9. Acceptance Criteria

- [ ] `echo-agent` ships `task_create`/`task_update`/`task_list` as first-class
      tools, gated on a supplied `RevisionedTaskStore`.
- [ ] `todo_write` is **deleted** from the framework.
- [ ] `echo-agent-app-core`'s `TaskRuntimeStore` implements
      `RevisionedTaskStore`; the three migrated tool structs are removed from
      app code.
- [ ] `register.rs` no longer calls `remove_tool("todo_write")`.
- [ ] EKO behavior unchanged: same tool names, same schemas, same revision
      semantics, same error messages, same summaries. Verified by ported unit
      tests + existing integration tests.
- [ ] TUI/GUI/CLI parity preserved (all three use the same framework tools).
- [ ] `echo-agent` verification matrix green (fmt check, clippy `-D warnings`,
      test workspace, no-default-features check).
- [ ] `echo-agent-cli` verification matrix green (fmt, clippy, test, GUI
      feature check, web-frontend build).
- [ ] No `TaskRuntimeStore` symbol leaks into `echo-agent` (grep-enforced).
- [ ] `MASTER-PLAN.md` updated with the new milestone.

---

## 10. Non-Goals

- Migrating `task_execute`, `create_complex_task`, `check_run_status`,
  `cancel_run` — they carry too much product logic; revisit only if a concrete
  reuse need appears.
- Changing the DAG kernel, `RuntimeDagController`, `PlanValidator`, or
  `TaskSpec`/`TaskExecution`/`TaskStatus` — settled by the 2026-07-27
  convergence.
- Adding `TaskGet` (Claude Code's fourth tool) — not currently used by EKO;
  can be added later as a trivial read tool on the same trait.
- Migrating any of the other ~45 app-core modules — audited, all correctly
  placed (adapter or product). See Appendix C summary.
- Introducing SQLite to EKO — out of scope (and forbidden by AGENTS.md).

---

## Appendix A — Decomposition evidence (summary)

Full per-tool decomposition with file:line evidence was produced by the
deep-dive. Headline verdicts:

| Tool | Verdict | Neutral core | Product adapter supplies |
|---|---|---|---|
| `task_create` | **SPLIT → migrate** | param validation, optimistic-concurrency assembly, summary | `ensure_run_exists` (hook), capability validation (in store impl), all persistence |
| `task_update` | **SPLIT → migrate** | op-discriminated patch parser, `PlanPatch` assembly, summary | capability validation, persistence |
| `task_list` | **SPLIT → migrate (near-trivial)** | run_id resolution, formatting | `list_tasks` read |
| `task_execute` | **STAY** | revision-match, lock, outcome formatting | unattended preflight, AgentHandle, reviewer LLM, execute_run DAG, register_run_cancellation |
| `create_complex_task` | **STAY** | (parsing only) | chat_resources, run_driver, DomainProfile, Subagent catalog |
| `check_run_status` | **STAY** | param parse, format | current_chat_resources + get_run |
| `cancel_run` | **STAY** | param parse, format | current_chat_resources + request_cancel |

The migrated three are the "clean切口" — their product coupling is entirely in
persistence + capability validation, both of which fit cleanly behind a trait.

## Appendix B — Store call surface (what the trait must expose)

Derived from grep of every `self.store.*` call in the three migrating tools:

| Method | Used by | Trait mapping |
|---|---|---|
| `get_plan(run_id) -> Option<TaskPlan>` | create, update, list, execute | `load_plan(run_id) -> Option<RevisionedPlan>` |
| `update_tasks(run_id, &TaskUpdateRequest) -> TaskPlan` | create, update | `apply_patch(run_id, PlanPatch) -> RevisionedPlan \| Conflict` |
| `attach_plan(&TaskPlan)` | create (initial) | Subsumed by `apply_patch` with `base_revision=0` semantics |
| `list_todos(run_id) -> Vec<TodoItem>` | list | `list_tasks(run_id) -> Vec<Task>` |
| `create_run(...)` + `set_run_attachments` + `transition_run(Running)` | create (bootstrap) | `ensure_run_exists(run_id, goal, ctx)` (single hook) |

Five store methods collapse to **four trait methods**. The product's richer
surface (`create_run`/`transition_run`/`note`/`get_summary`/`request_cancel`/
`register_run_cancellation`) is not needed by the migrating tools and stays
un-exposed by the trait.

## Appendix C — Other modules audited (all correctly placed)

For completeness, the broader audit (user's "not just tasks/") confirmed every
other app-core module is correctly layered:

- **ADAPTER** (correct trait seam): `subagent_prompt` (`SubagentPromptCompiler`),
  `chat_driver`/`run_driver` (call `execute_stream_*`), `FileConversationStore`
  (`ConversationStore`), `FileRuntimeStateStore` (`RuntimeStateStore`),
  `HitlDispatcher` (`HumanLoopProvider`), `TaskRuntimeContextProjector`
  (`PreModelContextProjector`), `scheduler` (re-exports framework `CronTask`).
- **PRODUCT** (genuinely EKO, stays): `subagent_loader` (`.eko/` convention +
  EKO frontmatter), `agent_pool` (multi-conversation desktop host),
  `tool_execution` (durable paged projection), `persistence` (CLI session DTOs),
  `SessionSearchEngine`, `DomainProfile`/`ProfileTemplate`/`AttendedMode`/
  `UnattendedWriteMode`, review gate, worktree policy, file-ownership policy,
  all product features (research/analysis/browser/coding-loop/skills-hub/etc.).

**No other DUPLICATE verdicts.** The layering discipline established by the
prior boundary plan is sound; this design closes the one remaining gap
(`todo_write` → modern `task_*`).
