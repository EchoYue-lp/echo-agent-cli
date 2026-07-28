# Task Tools Framework Migration: Revised Comprehensive Design

Date: 2026-07-28

Status: implemented and fully verified locally

Repositories:

- Framework: `echo-agent/`
- Product application: `echo-agent-cli/`

Implementation result (2026-07-28): the framework service, patch engine,
default in-memory tools, public registration function, EKO store/policy
adapters, LLM tool cutover, Tauri cutover, background initial-plan cutover, and
legacy tool/store API deletion are complete in the working tree. The framework,
application, no-default-feature, isolated framework feature, and GUI-only
submission matrices all pass with zero warning or failure.

## 1. Executive Decision

Move `task_create`, `task_update`, and `task_list` into the `echo-agent`
framework, but do not make persistence adapters implement task mutation
semantics.

The framework must own one authoritative task-revision service:

- parse the stable tool wire format;
- construct canonical `TaskSpec` / `TaskExecution` values;
- apply `Insert`, `Update`, `Skip`, and `Reorder` exactly once;
- enforce mutable-state restrictions;
- call the existing `PlanValidator`;
- calculate the next revision and generic patch effects;
- coordinate optimistic compare-and-swap commits;
- format tool results and errors.

Applications supply two thin adapters:

- `RevisionedTaskStore`: load one coherent graph and atomically commit an
  already-computed next revision;
- `TaskToolPolicy`: resolve the product scope, bootstrap a run, inject product
  metadata/defaults, and validate product capabilities.

EKO supplies file/event persistence, `DomainProfile`, Subagent/tool capability
validation, attachment bootstrap, and product projections through those
adapters. It must not retain a second patch engine or task validator.

`todo_write` is removed only in the same framework commit that installs a
per-Agent in-memory implementation of `task_create/update/list` by default.
The replacement therefore covers the framework's current zero-configuration
task-tracking use case instead of deleting a public API merely because EKO does
not use it.

This migration does not change the DAG executor, task dispatch, review,
worktree, cancellation, or acceptance paths.

## 2. Why The Previous Draft Was Not Implementable

The previous draft had the right ownership direction but five blocking flaws:

1. `RevisionedTaskStore::apply_patch` made every store reimplement revision,
   patch, and validation behavior. `InMemoryRevisionedTaskStore` and EKO would
   immediately become two mutation engines.
2. `apply_patch -> Result<_, PlanRevisionConflict>` could not represent missing
   runs/plans/tasks, invalid state, invalid patches, policy rejection, or
   backend failures.
3. A direct implementation on `TaskRuntimeStore` could not access the live
   `TaskCapabilityCatalog`, because that catalog is built from the Agent's
   current Subagent registry and registered tool names.
4. The proposed snapshot omitted `goal`, `assumptions`, `risks`, and
   `execution_mode`, so initial `task_create` could not round-trip the current
   EKO plan artifact.
5. `list_tasks -> Vec<Task>` discarded the revision that `task_list` must print.

The revised design fixes the boundary before any public API is added.

## 3. Implementation Gate: Existing Authorities

No new type or service may be implemented until it is checked against these
existing authorities.

### 3.1 Existing framework authorities to reuse

| Existing authority | Location | Decision |
|---|---|---|
| `TaskSpec` | `echo-orchestration/src/tasks/runtime.rs` | Reuse as the immutable executable specification |
| `TaskExecution` | same module | Reuse as mutable execution state |
| `TaskStatus` | same module | Reuse; do not add a CRUD status enum |
| `Task` | same module | Reuse as `TaskSpec + TaskExecution` |
| `TaskKind` | same module | Reuse its existing eight values in the v1 wire schema |
| `RuntimePlanSnapshot` | `runtime_executor.rs` | Reuse as the canonical `{revision, tasks}` runtime snapshot |
| `PlanValidator` | `planning/validator.rs` | Reuse for identity, dependency, cycle, depth, and retry validation |
| `RuntimeDagExecutor` | `tasks/runtime_executor.rs` | Do not modify |
| `RuntimeDagController` | same module | Do not replace or extend for CRUD |

`PlanSpec` remains the rich LLM authoring artifact. It is not reused as the
revisioned runtime graph because it contains `PlanTaskSpec`, authoring edges,
milestones, and authoring policy rather than canonical runtime `Task` values.

### 3.2 Existing EKO behavior to preserve

| Behavior | Current authority | Migration requirement |
|---|---|---|
| `task_create` one-or-batch schema | `task_tools.rs` | Preserve the exact v1 JSON schema |
| Existing graph append requires `base_revision` | `TaskCreateTool` | Preserve |
| Initial assumptions/risks/execution mode | `TaskPlan` | Preserve losslessly |
| Product run bootstrap | `TaskCreateTool::ensure_run_exists` | Move behind policy adapter |
| Default Subagent by domain/kind | `parse_plan_task` | Move behind policy adapter |
| Subagent/tool capability checks | `TaskCapabilityCatalog` | Move behind policy adapter |
| Atomic event/file commit | `TaskRuntimeStore` | Keep in EKO store adapter |
| GUI/Tauri `update_tasks` | Tauri command + store | Route through the same framework service |
| Todo/UI projections | `TaskPlan` / `TodoItem` | Keep as EKO projections |

### 3.3 Existing code that must not survive as a second authority

After all production callers have switched, remove:

- EKO `TaskCreateTool`, `TaskUpdateTool`, and `TaskListTool`;
- EKO's patch-application body in `TaskRuntimeStore::update_tasks`;
- EKO-local task tool schema/parser implementations that the framework now
  owns;
- production calls that directly mutate a revision through `attach_plan` or
  `update_tasks` instead of the framework service;
- framework `TodoWriteTool` and its process-global static task vector.

EKO projection DTOs may remain. A projection is not a second semantic
authority as long as it round-trips through the canonical framework task and
does not validate, schedule, or mutate independently.

## 4. Reference Implementations And Evidence Limits

### 4.1 Claude Code

Claude's Agent SDK documentation describes `TaskCreate`, `TaskUpdate`,
`TaskList`, and `TaskGet` as the replacement for `TodoWrite`, including task
dependencies/blockers:

<https://code.claude.com/docs/en/agent-sdk/todo-tracking>

This is strong evidence for:

- separate task tools instead of one action-switched `todo_write` tool;
- stable task identifiers;
- dependency-aware task relationships;
- queryable task state.

It is not evidence for EKO's revision/CAS patch protocol. Claude's documented
surface is item-oriented; EKO's atomic graph creation and revisioned patching
remain EKO requirements generalized into framework primitives.

### 4.2 Codex

`openai/codex#24547` proposes task/plan lifecycle hooks and an external plan
update API:

<https://github.com/openai/codex/issues/24547>

It is a public proposal, not an adopted Codex contract. It is retained only as
a directionally related reference and must not be cited as industry consensus.
The official Codex manual endpoint was unavailable during this revision, so no
additional Codex product behavior is asserted.

### 4.3 Other mature implementations already audited locally

The workspace contains previously audited implementations that support a
narrower architectural conclusion:

- OpenCode keeps Subagent invocation mechanics behind a task tool/service and
  product permissions outside the reusable core.
- DeepAgents separates stable task identity from individual asynchronous
  Subagent run identity.
- Hermes Kanban centralizes readiness, claims, completion, and dependencies in
  one task authority while UI/gateway layers consume projections.

These implementations support one lifecycle authority plus thin adapters.
They do not establish EKO's exact wire schema.

### 4.4 Resulting evidence statement

The defensible conclusion is:

> Separate task tools and stable task relationships are established patterns;
> EKO's revisioned graph protocol is a project requirement whose correctness
> must come from one framework implementation and repository tests.

The revised report does not use the phrase "industry consensus" for the full
EKO API.

## 5. Scope

### 5.1 In scope

- framework `task_create`, `task_update`, and `task_list` tools;
- their exact v1 JSON schemas and response formatting;
- raw wire DTOs and canonical patch DTOs;
- a pure task patch engine;
- a single `TaskRevisionService`;
- typed mutation/store/policy errors;
- a thin, object-safe `RevisionedTaskStore`;
- a thin, object-safe `TaskToolPolicy`;
- a per-Agent `InMemoryRevisionedTaskStore` and default policy;
- policy-gated manual progress transitions for the default lightweight task
  use case;
- framework default registration and `todo_write` deletion;
- EKO file/event store adapter and EKO policy adapter;
- switching LLM tools, GUI/Tauri update, and other production create/patch
  callers to the same service;
- schema, error, summary, CAS, round-trip, and surface-parity tests.

### 5.2 Out of scope

- changing `RuntimeDagExecutor`, `RuntimeDagController`, ready-frontier logic,
  claim semantics, revision safe points, retries, or deadlock handling;
- migrating `task_execute`, `create_complex_task`, `check_run_status`, or
  `cancel_run` into the framework;
- allowing EKO's formal PlanTask tools to write `Running`, `Completed`,
  `Failed`, `TimedOut`, or retry state; those transitions remain
  executor-owned. The default in-memory framework policy is allowed to expose
  restricted manual Pending/Running/Completed/Cancelled progress transitions
  for lightweight non-executed task tracking;
- moving EKO review, worktree, file ownership, memory bridge, DomainProfile
  routing, attachment storage, or UI projections into the framework;
- adding SQLite to EKO;
- collapsing `PlanTaskKind`, `SuggestedTask`, or execution-summary projections;
- adding `task_get` in this migration. It can be added later as a read-only
  service call, but this report no longer claims exact Claude tool parity.

## 6. Layering Decision

### 6.1 Framework: reusable mechanism

The framework owns:

- tool names, descriptions, base wire schemas, policy-gated schema
  composition, and input parsing;
- canonical task patch types;
- patch application and state restrictions;
- structural validation through the existing `PlanValidator`;
- optimistic revision coordination;
- generic patch effects;
- stable error categories and tool result formatting;
- default in-memory persistence and default task scope;
- the generic manual-progress transition mechanism used only when policy
  enables it;
- registration factories for `ReactAgent` consumers.

### 6.2 EKO: product policy and persistence

EKO owns:

- `events.jsonl`, `plan.json`, and `run-state.json`;
- TaskRun creation, transition, conversation/message binding, and attachments;
- `DomainProfile` and default Subagent routing;
- validation against live registered Subagents and tools;
- the `parallel_group` task-input schema extension and its decoding;
- plan id and EKO graph metadata;
- `parallel_group`/`sort_order` projection rules;
- product event emission;
- Tauri/GUI/TUI/CLI/channel projection;
- all execution triggers and execution policy.

### 6.3 Adapter boundary

EKO provides two concrete adapters:

```text
EkoRevisionedTaskStore
  owns Arc<TaskRuntimeStore>
  implements load + compare_and_commit only

EkoTaskToolPolicy
  owns Arc<TaskRuntimeStore> + Arc<TaskCapabilityCatalog>
  resolves/bootstrap scope, defaults Subagent, injects EkoTaskMetadata,
  validates product capabilities
```

Do not implement `RevisionedTaskStore` directly on `TaskRuntimeStore`. The
separate wrapper makes the boundary explicit and prevents persistence from
acquiring Agent registry/policy dependencies.

## 7. Target Architecture

```text
LLM / GUI / Tauri / CLI
          |
          v
framework TaskRevisionService  <--- single mutation authority
  - wire validation
  - TaskPatchEngine
  - PlanValidator
  - revision calculation
  - error/summary contract
          |
          +-------------------+
          |                   |
          v                   v
RevisionedTaskStore      TaskToolPolicy
load + CAS commit        scope/bootstrap/defaults/capabilities
          |                   |
          +---------+---------+
                    v
              EKO adapters
                    |
                    v
      events.jsonl / plan.json / run-state.json
                    |
                    v
        RuntimeDagController adapter
                    |
                    v
        existing RuntimeDagExecutor
```

The mutation service and DAG executor share canonical `Task` values and
`PlanValidator`, but neither calls or owns the other.

## 8. Canonical Data Model

### 8.1 Reused types

No new task/status/runtime model is introduced:

```rust
TaskSpec       // immutable specification
TaskExecution  // mutable execution state
TaskStatus     // shared lifecycle
Task           // spec + execution
TaskKind       // existing closed framework enum
RuntimePlanSnapshot { revision, tasks }
```

### 8.2 Revision envelope

`RuntimePlanSnapshot` intentionally contains only execution-relevant data. The
task tools also need generic graph context and lossless product extensions, so
the service wraps rather than replaces it:

```rust
pub struct RevisionedTaskGraph {
    pub snapshot: RuntimePlanSnapshot,
    pub context: TaskGraphContext,
}

pub struct TaskGraphContext {
    pub goal: String,
    pub assumptions: Vec<String>,
    pub risks: Vec<String>,
    pub execution_mode: TaskGraphExecutionMode,
    pub metadata: serde_json::Value,
}

pub enum TaskGraphExecutionMode {
    Parallel,
    Sequential,
}
```

This is a revision envelope, not a third scheduler model. EKO maps it losslessly
to and from `TaskPlan`:

- `context.metadata.plan_id` -> `TaskPlan.plan_id`;
- `context.metadata.domain_profile` -> `TaskPlan.domain_profile`;
- `context` typed fields -> goal/assumptions/risks/execution mode;
- `snapshot.tasks` -> checked `Task <-> PlanTask` conversion.

### 8.3 Stable v1 wire DTOs

The first migration preserves EKO's current external contract:

- create-task field remains `subagent`;
- update-patch field remains `agent_role`;
- `kind` remains the current eight-value enum;
- create retains `assumptions`, `risks`, and `execution_mode`;
- EKO's composed task input retains `parallel_group`;
- exactly one of `task` or `tasks` is required;
- existing-graph create still requires `base_revision`;
- update still requires `base_revision`, non-empty `reason`, and operations.

The wire DTO is intentionally separate from canonical `TaskSpec` because an
omitted `subagent` requires product defaulting before `agent_role` exists.
Product-specific task fields are collected in an opaque extension value rather
than becoming framework fields.

```rust
pub struct TaskDraft {
    pub id: String,
    pub title: String,
    pub description: String,
    pub kind: TaskKind,
    pub subagent: Option<String>,
    pub depends_on: Vec<TaskId>,
    pub files: Vec<String>,
    pub allowed_tools: Vec<String>,
    pub required_artifacts: Vec<String>,
    pub execution_checks: Vec<String>,
    pub acceptance_criteria: Vec<String>,
    pub max_retries: u32,
    pub extensions: serde_json::Value,
}
```

The framework owns the base task schema. `TaskToolPolicy` may contribute a
declarative set of additional task properties, and the framework composes them
into the final `additionalProperties: false` schema. EKO contributes only:

```json
{
  "parallel_group": { "type": "string" }
}
```

The framework parser collects configured extension fields into
`TaskDraft.extensions`; it never names or interprets `parallel_group`. This
preserves EKO's exact current schema without promoting an EKO scheduling hint
into the reusable task model. The default framework policy contributes no task
schema extensions.

The framework also owns an optional core `set_status` update-operation schema.
It is included only when policy enables manual progress updates. The default
in-memory policy enables it so the new API covers lightweight progress
tracking; EKO disables it so the existing formal PlanTask schema and executor
authority remain unchanged.

Do not expose an arbitrary string `kind` while canonical `TaskSpec.kind` is a
closed `TaskKind`; that would create a schema the service cannot represent.

### 8.4 Canonical patch DTOs

```rust
pub struct TaskPlanPatch {
    pub base_revision: u64,
    pub reason: String,
    pub operations: Vec<TaskPlanPatchOp>,
}

pub enum TaskPlanPatchOp {
    Insert {
        after_task_id: Option<TaskId>,
        task: TaskSpec,
    },
    Update {
        task_id: TaskId,
        patch: TaskSpecPatch,
    },
    Skip {
        task_id: TaskId,
    },
    Reorder {
        task_ids: Vec<TaskId>,
    },
    SetStatus {
        task_id: TaskId,
        status: TaskStatus,
    },
}

pub struct TaskSpecPatch {
    pub title: Option<String>,
    pub description: Option<String>,
    pub kind: Option<TaskKind>,
    pub agent_role: Option<String>,
    pub depends_on: Option<Vec<TaskId>>,
    pub files: Option<Vec<String>>,
    pub allowed_tools: Option<Vec<String>>,
    pub required_artifacts: Option<Vec<String>>,
    pub execution_checks: Option<Vec<String>>,
    pub acceptance_criteria: Option<Vec<String>>,
    pub max_retries: Option<u32>,
}
```

Product metadata is not accepted as a free-form LLM patch. EKO metadata is
derived by policy from typed inputs and the current run, preventing arbitrary
metadata from bypassing product invariants.

## 9. Store And Policy Contracts

### 9.1 Thin `RevisionedTaskStore`

```rust
#[async_trait::async_trait]
pub trait RevisionedTaskStore: Send + Sync {
    async fn load(
        &self,
        scope_id: &str,
    ) -> Result<Option<RevisionedTaskGraph>, RevisionedTaskStoreError>;

    async fn compare_and_commit(
        &self,
        scope_id: &str,
        commit: TaskGraphCommit,
    ) -> Result<RevisionedTaskGraph, RevisionedTaskStoreError>;
}

pub struct TaskGraphCommit {
    /// None means "create only if absent". Some(N) means "commit only if the
    /// current revision is exactly N".
    pub expected_revision: Option<u64>,
    pub next: RevisionedTaskGraph,
    pub reason: String,
    pub effects: TaskPatchEffects,
}

pub struct TaskPatchEffects {
    pub inserted_task_ids: Vec<TaskId>,
    pub updated_task_ids: Vec<TaskId>,
    pub skipped_task_ids: Vec<TaskId>,
    pub reset_task_ids: Vec<TaskId>,
    pub progressed_task_ids: Vec<TaskId>,
    pub reordered: bool,
}
```

Store obligations are deliberately narrow:

1. load a coherent snapshot;
2. serialize compare-and-commit for one scope;
3. reject absent/present or revision races as `Conflict`;
4. persist the already-computed candidate and effects atomically;
5. return the committed projection.

The Store must not parse patch operations, choose defaults, perform capability
validation, or implement DAG validation.

### 9.2 Typed store errors

```rust
pub enum RevisionedTaskStoreError {
    NotFound { scope_id: String },
    Conflict {
        expected: Option<u64>,
        current: Option<u64>,
    },
    Rejected { message: String },
    Backend { message: String },
}
```

`Rejected` is reserved for persistence-side invariants that must be checked in
the same lock/transaction, such as EKO refusing to mutate a terminal TaskRun.
It is not a general policy escape hatch.

### 9.3 Product policy adapter

```rust
#[async_trait::async_trait]
pub trait TaskToolPolicy: Send + Sync {
    fn task_input_schema_extensions(&self) -> serde_json::Map<String, serde_json::Value>;

    fn allow_manual_progress_updates(&self) -> bool;

    async fn resolve_scope(
        &self,
        context: &ToolContext,
    ) -> Result<String, TaskPolicyError>;

    async fn ensure_scope(
        &self,
        scope_id: &str,
        input: &TaskCreateInput,
        context: &ToolContext,
    ) -> Result<(), TaskPolicyError>;

    async fn prepare_task(
        &self,
        scope_id: &str,
        draft: &TaskDraft,
        position: usize,
    ) -> Result<PreparedTaskPolicy, TaskPolicyError>;

    async fn prepare_initial_context(
        &self,
        scope_id: &str,
        input: &TaskCreateInput,
    ) -> Result<TaskGraphContext, TaskPolicyError>;

    async fn finalize_task_metadata(
        &self,
        scope_id: &str,
        task_id: &str,
        position: usize,
        metadata: serde_json::Value,
    ) -> Result<serde_json::Value, TaskPolicyError>;

    async fn validate_candidate(
        &self,
        scope_id: &str,
        tasks: &[Task],
    ) -> Result<(), TaskPolicyError>;
}

pub struct PreparedTaskPolicy {
    pub agent_role: String,
    pub metadata: serde_json::Value,
}
```

Policy failures use a separate typed boundary:

```rust
pub enum TaskPolicyError {
    ScopeUnavailable { message: String },
    Rejected { message: String },
    Backend { message: String },
}
```

EKO uses these hooks as follows:

- schema extensions: add the existing `parallel_group` property;
- manual progress updates: disabled;
- scope: `ToolContext.run_id`, then formal id from `turn_id`, then current EKO
  task-local run id;
- bootstrap: current conversation/message/resources, attachments, run creation,
  transition, and product event;
- task preparation: choose default Subagent from DomainProfile + TaskKind and
  inject `EkoTaskMetadata`;
- metadata finalization: update `sort_order` metadata from vector order;
- candidate validation: read-only validation of every Subagent/tool
  capability.

The framework, not policy, constructs `TaskSpec` from `TaskDraft` plus the
returned `agent_role` and metadata. Policy can normalize only metadata and can
inspect a candidate only through `&[Task]`; its interface cannot rewrite
generic dependencies or statuses. It may not apply operations, increment
revisions, detect cycles, or commit persistence.

The schema-extension hook is declarative. It cannot replace the core schema,
change required core fields, or enable arbitrary additional properties.

When manual progress is enabled, the framework exposes only these wire values:

```text
pending -> TaskStatus::Pending
in_progress -> TaskStatus::Running
completed -> TaskStatus::Completed
cancelled -> TaskStatus::Cancelled
```

`TaskPatchEngine` treats a same-state update as idempotent and otherwise calls
the existing `TaskStatus::transition_to`; it does not introduce a second status
transition table. A normal lightweight flow is Pending -> Running -> Completed,
with cancellation where the shared state machine permits it. The tool does not
expose Failed, Blocked, TimedOut, Retrying, Paused, claims, retry counters, or
result details. EKO never enables this path.

### 9.4 Unified service errors

```rust
pub enum TaskRevisionError {
    InvalidInput { message: String },
    GraphNotFound { scope_id: String },
    TaskNotFound { task_id: TaskId },
    RevisionConflict {
        expected: Option<u64>,
        current: Option<u64>,
    },
    InvalidPatch { message: String },
    PolicyRejected { message: String },
    StoreRejected { message: String },
    Backend { message: String },
}
```

Framework tools convert these categories to the existing EKO-facing strings.
Golden tests lock exact messages. Store adapters never fabricate a conflict for
an unrelated failure.

### 9.5 `TaskRevisionService`

The service is the only component that combines Store, policy, patch engine,
and validator:

```rust
pub struct TaskRevisionService {
    store: Arc<dyn RevisionedTaskStore>,
    policy: Arc<dyn TaskToolPolicy>,
    validator: PlanValidator,
}
```

Its public operations are intentionally split by input level:

```rust
load(scope_id)
create_from_tool(input, tool_context)
update_from_tool(input, tool_context)
create_prepared(scope_id, context, tasks, reason)
apply_patch(scope_id, canonical_patch)
```

The two tool methods own wire parsing/scope resolution. The prepared methods
let GUI/Tauri and EKO planning services reuse the same revision/validation/CAS
path after converting their product DTOs. Neither path bypasses
metadata finalization, read-only product validation, `PlanValidator`, or
compare-and-commit.

## 10. The Single Patch Engine

`TaskPatchEngine` is a pure framework component. Both the in-memory service and
EKO service call this same implementation.

For `apply(current, patch)` it performs, in order:

1. require `base_revision >= 1`;
2. require a non-empty reason and operation list;
3. reject a stale base revision before mutation;
4. apply operations in request order;
5. reject duplicate inserts and missing `after_task_id`/`task_id` targets;
6. allow specification updates only for Pending or Blocked tasks;
7. reset an updated Blocked task to Pending and clear its claim/detail;
8. allow Skip only for Pending or Blocked tasks;
9. apply SetStatus only when manual progress is policy-enabled and the existing
   `TaskStatus` transition is valid;
10. require Reorder to contain every task id exactly once;
11. let policy refresh only per-task metadata from canonical vector order;
12. run read-only product candidate validation;
13. call the existing `PlanValidator` on canonical tasks;
14. produce revision `current + 1` with checked/saturating arithmetic;
15. return the candidate plus `TaskPatchEffects`.

The service then calls `compare_and_commit(expected=current.revision)`. A race
between load and commit returns a typed conflict; the service does not retry a
model-authored patch against a different revision.

Initial creation is separate from patching:

```text
load -> absent
ensure_scope
prepare tasks and full graph context
PlanValidator
build revision 1
compare_and_commit(expected_revision=None)
```

There is no hidden `base_revision == 0` overload. Creation and update therefore
have distinct, testable contracts.

## 11. Tool Behavior

### 11.1 `task_create`

1. Parse exactly one of `task` or non-empty `tasks`.
2. Resolve and bootstrap the product scope through policy.
3. Load the current graph.
4. Convert every `TaskDraft` through `prepare_task`.
5. If absent, build the complete initial context and commit revision 1.
6. If present, require `base_revision`, convert tasks into Insert operations,
   and call the normal patch path.
7. Return the current EKO summary text unchanged.

For an existing graph, create-level assumptions/risks/execution mode remain
ignored exactly as they are today; only inserted tasks and reason participate.

### 11.2 `task_update`

1. Parse `base_revision`, `reason`, and operations.
2. Resolve scope and load the graph.
3. Convert Insert drafts through policy; convert Update fields into typed
   `TaskSpecPatch`.
4. Call `TaskRevisionService::apply_patch`.
5. On conflict, preserve the current `Failed to update tasks: ... expected ...
   current ...` error shape; callers can reload through `task_list` before
   submitting a new patch.
6. Return the current committed-revision summary unchanged.

Under EKO policy the tool cannot mark execution success/failure. `Skip` remains
a planning decision, while formal execution outcomes remain
`RuntimeDagController` writes. Under the default in-memory policy, the same
tool name additionally exposes restricted `set_status` for lightweight manual
progress tracking.

### 11.3 `task_list`

`task_list` calls only `TaskRevisionService::load` and formats both revision and
tasks from the same coherent `RevisionedTaskGraph`. There is no separate
`list_tasks` Store method and no chance to join a revision from one read with
tasks from another.

Existing empty/error/success text remains unchanged.

## 12. Replacing `todo_write` Correctly

The existing `todo_write` is always registered and uses one process-global
`LazyLock<Mutex<Vec<_>>>`. The replacement must therefore work even when a
consumer supplies no custom Store or run id.

### 12.1 Default framework behavior

Every `ReactAgent` receives, by default:

- its own `Arc<InMemoryRevisionedTaskStore>`;
- `DefaultTaskToolPolicy`;
- `task_create`, `task_update`, and `task_list`.

The default scope resolver uses:

1. `ToolContext.run_id` when provided;
2. `ToolContext.conversation_id` when provided;
3. an Agent-instance scope id created by the builder.

It does not require task-local state, so direct `Tool::execute` with an empty
context remains usable. Per-Agent storage also removes the current accidental
cross-Agent global task list.

The default policy uses explicit `subagent` when supplied and otherwise the
framework role `default`. It performs structural validation but no EKO
capability validation.

It also enables the framework-owned `set_status` operation so a lightweight
task can move through Pending -> Running -> Completed or be Cancelled without
invoking the DAG executor. These tasks still use canonical `TaskStatus` and the
same revision/CAS service; there is no separate Todo model or Store.

### 12.2 Public feature surface

The task relation API must be available in a default framework build because
it replaces an always-on tool. The root facade must therefore expose the
canonical task relation types without requiring the optional background-task
tools.

The existing `tasks` feature may continue to control background spawn/check
tools, but it cannot gate the replacement CRUD tools. This feature-topology
change requires the repository's full feature matrix.

### 12.3 Deletion criterion

Delete `todo_write` only when one framework commit proves all of the following:

- the three new tools are registered in the default Agent configuration;
- create/update/list work without an injected Store or ToolContext run id;
- the default update schema supports restricted manual progress transitions;
- state is isolated per Agent instance;
- a custom Store/policy can replace the defaults;
- no `todo_write` symbol remains in framework registration, prompts, tests, or
  docs.

The removal is an intentional framework breaking change. It is justified by
behavioral coverage and a better authority model, not by counting current EKO
callers. The changelog must note that ids, revision semantics, and isolation
differ from the old process-global scratchpad.

## 13. Registration

The split crate must not depend upward on `ReactAgent`, and `ReactAgent` does
not currently implement `ToolRegistrar`. Avoid a free function that accepts
`&mut ToolRegistrar`.

`echo-orchestration` exposes tool factories:

```rust
pub fn build_task_tools(
    service: Arc<TaskRevisionService>,
) -> Vec<Box<dyn Tool>>;

pub fn build_task_create_tool(...) -> Box<dyn Tool>;
pub fn build_task_update_tool(...) -> Box<dyn Tool>;
pub fn build_task_list_tool(...) -> Box<dyn Tool>;
```

The root `echo-agent` crate owns `ReactAgent` integration:

- builder path: create the default in-memory service or use a supplied custom
  service, then call `ReactAgent::add_tools`;
- post-hoc path: EKO builds an EKO service and replaces the same three tool
  names before the Agent serves a turn.

Using `ReactAgent::add_tools` preserves `enable_tool` and `allowed_tools`
behavior. Registration tests must prove that replacement does not leave two
definitions or bypass allowed-tool filtering.

## 14. EKO Adapter Details

### 14.1 `EkoRevisionedTaskStore`

`load`:

- call the existing file/event read projection;
- combine plan specification and execution projection into canonical `Task`;
- build `RuntimePlanSnapshot`;
- map plan-level fields into `TaskGraphContext`;
- fail on lossy or inconsistent conversion.

`compare_and_commit`:

- acquire the existing per-run lock;
- load the authoritative current revision inside the lock;
- enforce terminal-run persistence invariant;
- compare absent/present/revision with `expected_revision`;
- convert the already-validated candidate to EKO projections;
- append the existing revision event with reason and effects;
- rebuild `plan.json` and execution projections atomically;
- return the committed graph.

It must not call EKO's old `update_tasks` patch body.

### 14.2 `EkoTaskToolPolicy`

The policy owns the live `TaskCapabilityCatalog`, which is constructed at Agent
registration from:

- the registered Subagent snapshot;
- current Agent tool names.

It also owns the `TaskRuntimeStore` reference needed to read DomainProfile and
bootstrap the TaskRun. This is product policy, not persistence abstraction.

`validate_candidate` validates every candidate task after all operations so
updates cannot introduce an unknown Subagent/tool indirectly.
`finalize_task_metadata` can update only EKO extension metadata; neither hook
can rewrite generic dependency or status semantics.

### 14.3 Attachments and task-local context

Do not add EKO attachment types to framework `ToolContext`. EKO's policy may
read current chat resources/task-local state when bootstrapping, while the
framework passes the ordinary `ToolContext` it already owns.

This preserves GUI/TUI/CLI/channel behavior without polluting the reusable
framework context type with an EKO storage DTO.

### 14.4 GUI/Tauri and non-tool callers

Before this migration, the Tauri `update_tasks` command called
`TaskRuntimeStore::update_tasks` directly and product planning paths attached
initial plans directly. Migrating only the LLM tools would therefore have been
incomplete.

All production mutation entry points must call `TaskRevisionService`:

- framework LLM task tools;
- Tauri/GUI `update_tasks`;
- chat/task service initial plan materialization;
- any executor-side dynamic revision insertion.

Tauri may keep EKO-generated TypeScript request/response DTOs as wire
projections, but the command converts them once and delegates to the framework
service. It may not apply or validate operations itself.

## 15. Migration Plan

Each phase must compile, switch a real path or delete replaced logic, and be
committed in its own repository. Framework commits land before application
commits.

### Phase 1: atomic framework replacement (`echo-agent`) - completed

Implement in one framework commit:

- revision envelope, wire DTOs, patch DTOs, effects, and typed errors;
- pure `TaskPatchEngine`;
- thin `RevisionedTaskStore` and `TaskToolPolicy`;
- `TaskRevisionService`;
- `InMemoryRevisionedTaskStore` and default policy;
- framework `task_create/update/list` tools;
- default ReactAgent registration and custom-service builder path;
- always-available public task relation facade;
- deletion of `TodoWriteTool` and all `todo_write` registration/docs/tests.

This phase switches the framework's real default task path. It does not leave
an unused framework service beside `todo_write`.

Required tests include default/no-context operation, Agent isolation, custom
Store replacement, complete patch semantics, CAS races, schema goldens, and
exact summaries/errors.

### Phase 2: EKO read adapter and `task_list` cutover (`echo-agent-cli`) - completed

- add `EkoRevisionedTaskStore` and checked graph conversions;
- add `EkoTaskToolPolicy` with scope/bootstrap hooks;
- construct one shared `TaskRevisionService` for the Agent/runtime;
- replace only `task_list` with the framework implementation;
- delete EKO `TaskListTool` and its duplicate formatting logic;
- remove now-obsolete `remove_tool("todo_write")` calls.

This phase proves coherent revision/task reads and switches one production path
without changing mutation behavior yet.

### Phase 3: EKO LLM mutation cutover (`echo-agent-cli`) - completed

- register framework `task_create` and `task_update` using the EKO service;
- delete EKO `TaskCreateTool` and `TaskUpdateTool`;
- delete duplicate tool schemas, parsers, summaries, and error mapping;
- keep `TaskCapabilityCatalog` only in `EkoTaskToolPolicy`;
- port existing tool tests as framework/service integration tests.

At this point all LLM task relation calls use the framework authority. The four
product execution/control tools remain unchanged.

### Phase 4: all remaining mutation callers and store cleanup (`echo-agent-cli`) - completed

- route Tauri/GUI `update_tasks` through `TaskRevisionService`;
- route production initial-plan materialization through the service's prepared
  create path;
- route executor-side dynamic insertion through the service;
- replace the old Store patch body with the low-level CAS commit primitive;
- delete public `TaskRuntimeStore::update_tasks` and obsolete app patch types
  once every caller is converted;
- keep `attach_plan` only if it is a private persistence helper with no
  validation/mutation semantics; otherwise delete it too.

This phase removes the final second patch authority.

### Phase 5: convergence audit and archive (`echo-agent-cli`) - complete

- run whole-repository grep gates;
- update `docs/MASTER-PLAN.md` with the authoritative paths and completed
  deletion targets;
- update boundary/deep-dive docs that still describe framework `todo_write`;
- run all framework/application/GUI/frontend verification gates;
- inspect disk usage and clean only if AGENTS.md thresholds require it.

No optional type cleanup is mixed into this migration.

## 16. Verification

### 16.1 Framework behavioral tests

- exact create/update/list JSON schema snapshots;
- one task and atomic batch creation;
- invalid `task` + `tasks` combinations;
- initial context preservation;
- existing graph requires base revision;
- duplicate/missing insert target;
- pending/blocked update restrictions;
- blocked update resets Pending and clears claim/detail;
- skip restrictions;
- default-policy manual Pending/Running/Completed/Cancelled transitions;
- EKO policy omits and rejects manual status operations;
- reorder exact-set validation;
- dangling dependency/cycle/identity validation through `PlanValidator`;
- stale load and commit-time CAS conflict;
- backend, policy, not-found, invalid-patch, and conflict error formatting;
- UTF-8 task titles/descriptions in summaries and errors;
- task_list reads revision and tasks from one snapshot;
- default ToolContext-free scope;
- per-Agent state isolation;
- custom service replacement;
- `todo_write` absent and new tools present in default Agent.

### 16.2 EKO adapter tests

- every `TaskPlan` plan-level field round-trips;
- every `TaskSpec` and `TaskExecution` field round-trips;
- `EkoTaskMetadata` domain/parallel group/sort order round-trips;
- malformed metadata returns an error instead of defaulting;
- default Subagent follows DomainProfile + TaskKind;
- explicit Subagent is preserved;
- unknown Subagent/tool is rejected;
- task-control tools cannot be delegated to a Subagent;
- attachment/conversation/message bootstrap is unchanged;
- terminal TaskRun mutation is rejected inside the commit lock;
- event payload and file projections remain unchanged;
- Tauri and LLM updates produce identical committed revisions;
- concurrent callers produce exactly one winner and one typed conflict.

### 16.3 Grep gates

Framework:

```bash
rg -n "todo_write|TodoWriteTool" src echo-* docs
rg -n "TaskRuntimeStore|DomainProfile|EkoTask" echo-orchestration src
```

Expected: zero semantic leaks; historical migration docs may be explicitly
excluded or updated.

Application production code:

```bash
rg -n "struct Task(Create|Update|List)Tool" echo-agent-app-core/src
rg -n "\.update_tasks\(|fn update_tasks\(" echo-agent-app-core/src src/tauri
rg -n "remove_tool\(\"todo_write\"\)" echo-agent-app-core/src src/tauri
```

Expected: no duplicate tool structs, patch engine, or obsolete removal call.
The Tauri command name `update_tasks` may remain as an external IPC name, but
its body must delegate directly to `TaskRevisionService`.

### 16.4 Framework submission gate

```bash
cd echo-agent
cargo fmt --all
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo clippy --workspace --lib --bins --all-features --locked -- \
  -D clippy::unwrap_used \
  -D clippy::expect_used \
  -D clippy::panic \
  -D clippy::unreachable
cargo test --workspace --all-targets --all-features --locked
cargo check --workspace --lib --no-default-features --locked
./scripts/verify-all-crates.sh --feature-matrix
```

The feature matrix is mandatory because the migration changes public API,
feature visibility, and default tool registration.

### 16.5 Application submission gate

```bash
cd echo-agent-cli
cargo fmt --all
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
cargo check -p echo-agent-app-core --no-default-features --locked
cargo check --no-default-features --features gui --bin echo-agent-tauri
cargo test --no-default-features --features gui

cd web-frontend
npx prettier --check "src/**/*.{ts,tsx}"
npm test
npm run build
```

Before commit, also verify no Cargo manifest contains a worktree or absolute
user path.

## 17. Acceptance Criteria

- [x] One framework `TaskRevisionService` owns all create/patch/list semantics.
- [x] `RevisionedTaskStore` performs only coherent load and atomic CAS commit.
- [x] `TaskToolPolicy` performs only scope/bootstrap/defaulting/product checks.
- [x] Framework and EKO do not contain parallel production patch engines.
- [x] Existing `TaskSpec`, `TaskExecution`, `TaskStatus`, `TaskKind`,
      `RuntimePlanSnapshot`, and `PlanValidator` remain canonical.
- [x] Initial goal/assumptions/risks/execution mode round-trip losslessly.
- [x] EKO v1 tool names and schemas are preserved by the policy-composed tools.
- [x] `task_list` formats one coherent revision snapshot.
- [x] Default framework Agents expose task_create/update/list without custom
      configuration or a run id.
- [x] Default framework task_update covers lightweight manual progress, while
      EKO formal task_update cannot forge executor outcomes.
- [x] `todo_write` is deleted only after that default replacement is active.
- [x] EKO task tools, Tauri/GUI updates, and production initial-plan callers use the
      same service.
- [x] Task execution statuses remain executor-owned.
- [x] TUI, GUI, CLI, and channels receive the same task capability surface.
- [x] No EKO persistence, DomainProfile, attachment, worktree, reviewer, or UI
      type leaks into `echo-agent`.
- [x] No SQLite dependency/feature is added to EKO.
- [x] All applicable verification and feature matrices pass with zero warning
      or failure.
- [x] `MASTER-PLAN.md` records the final authoritative path and deleted legacy
      path after implementation completes.

## 18. Risk Register

| Risk | Required mitigation |
|---|---|
| Store adapter starts applying operations | Keep `compare_and_commit` input as an already-built candidate; unit-test that both stores use the same engine |
| Product policy becomes a second validator | Permit capability/metadata checks only; structural checks remain `PlanValidator` |
| Initial plan data is lost | Typed `TaskGraphContext` plus extension metadata and field-level round-trip tests |
| Race between load and commit | Mandatory Store CAS under the existing per-run lock; no model-authored auto-retry |
| Default framework build loses task tracking | Register per-Agent in-memory task tools and manual progress updates before deleting `todo_write` |
| New framework tools collide with EKO tools | Replace identical names before the first served turn; assert one definition per name |
| GUI bypasses the framework service | Route Tauri and every production mutation call through the same service; grep direct Store mutation |
| Metadata bag becomes an untyped escape hatch | LLM wire does not accept arbitrary metadata; policy alone constructs EKO metadata |
| Error behavior regresses | Typed error taxonomy plus exact golden message tests |
| Feature combinations break | Mandatory no-default and feature-matrix verification |

## 19. Final Ownership Matrix

| Capability | Framework | EKO | Notes |
|---|---|---|---|
| Tool schemas/parsing | Base authority | Declarative extension only | Composed EKO schema preserves the exact v1 contract |
| Task/status/kind model | Authority | Checked projection | No third runtime model |
| Patch operation semantics | Authority | None | One pure engine |
| Manual progress transitions | Policy-gated mechanism | Disabled | Default in-memory policy only |
| DAG structural validation | Authority | None | Existing `PlanValidator` |
| Revision calculation | Authority | None | Store only compares/commits |
| Atomic file/event commit | Interface | Authority | `EkoRevisionedTaskStore` |
| Run bootstrap | Hook | Authority | `EkoTaskToolPolicy` |
| Domain/Subagent/tool validation | Hook | Authority | Product catalog |
| Default framework persistence | Authority | None | Per-Agent in-memory Store |
| DAG execution | Existing authority | Controller adapter | Unchanged |
| Review/worktree/file ownership | None | Authority | Unchanged |
| GUI/TUI/CLI/channel projection | None | Authority | Same service underneath |

## 20. Final Decision

Proceed with the migration only after adopting this corrected boundary:

> The framework owns one task-revision service and one patch engine. Stores
> persist an already-computed candidate through CAS. EKO supplies persistence
> and product policy through separate thin adapters. All task mutation callers,
> including GUI/Tauri, use the same service. `todo_write` is deleted only when
> the new API is the default zero-configuration framework path.

This produces the intended final model: lightweight progress tasks and
executable PlanTask DAG nodes are one formal task-relationship API, while
execution authority and EKO product policy remain correctly separated.
