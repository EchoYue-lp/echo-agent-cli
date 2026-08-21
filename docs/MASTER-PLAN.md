# EKO Master Plan

Last updated: 2026-08-20

This file is the cross-session source of truth for the coding, data analysis,
academic research, and medical research expansion. Detailed design rationale
stays in the milestone documents; this file records only decisions, status,
evidence, and the next bounded step.

## Product Invariants

- EKO is a local personal assistant. Do not add online multi-tenant security or
  permission models to interactive local tools.
- The product model is `TaskRun -> PlanTask -> SubagentRun`; all execution
  roles, instances, events, and concurrency slots use Subagent terminology.
- EKO persists conversations, analyses, research records, and projections as
  ordinary files. `echo-agent-cli` does not enable SQLite.
- TUI, GUI, CLI, and channels share the same Agent capabilities and differ only
  in input, rendering, and event projection.
- Plans, scripts, sources, evidence, and reports are inspectable artifacts. They
  are not hidden runtime states.

## Architecture Boundary

| Responsibility | Owner |
|---|---|
| ReAct, tools, `run_code`, LSP client/discovery, AST symbol extraction, reusable scholarly API clients, workflow primitives | `echo-agent` |
| Domain routing, TaskRuntime, file-backed analysis/research contracts, IPC, TUI/GUI/CLI/channel surfaces | `echo-agent-cli` |
| Statistical inference | Persisted Python/R using SciPy, statsmodels, or mature R packages |
| Exploratory statistics | Framework descriptive tooling, explicitly not formal inference |

## Milestone Status

| Milestone | Status | Evidence |
|---|---|---|
| DomainProfile propagation and complex Run plan creation | Complete | `docs/2026-07-17-domain-subagent-orchestration.md` |
| Statistical inference correctness | Complete | `docs/2026-07-18-statistical-inference-correctness.md`; framework commit `5a2c8a8` |
| Persisted script execution | Complete | framework commit `1e71b9a` |
| File-backed analysis workbench and surface parity | Complete | `docs/2026-07-18-file-backed-analysis-workbench.md`; application commit `9699670` |
| SourceRecord/EvidenceRecord and paper IPC | Complete | `echo-agent-app-core/src/research.rs`; application commit `dffe9d6` |
| PaperPanel, evidence matrix, and systematic review workbench | Complete | `web-frontend/src/components/papers/ReviewWorkbench.tsx`; no browser `localStorage` persistence |
| Medical PICO, screening, RoB, GRADE, and PRISMA | Complete | `ReviewRecord` contract and `ReviewWorkbench` medical mode |
| LSP automatic discovery and AST-aware repo map | Complete | framework commit `1c6442e` |
| Legacy framework DataPipeline migration | Complete | `../echo-agent/echo-orchestration/src/workflow/pipelines/data_pipeline.rs`; one tool-capable, code-first file contract |
| Automatic scholarly ingestion and open metadata clients | Complete | `../echo-agent/echo-tools/src/research/clients.rs`; `echo-agent-app-core/src/research_connectors.rs` |
| Zotero import/export and Europe PMC enrichment | Complete | PaperPanel connector controls; PMID/PMCID citation, reference, entity, and full-text enrichment |
| Citation audit and systematic-review report export | Complete | File-backed Markdown, JSON, CSV, BibTeX, and RIS artifacts under each review's `reports/` directory |
| Medical PECO and applicability risks | Complete | Structured harms, contraindications, conflicts of interest, guideline conflicts, and extrapolation limits |
| Global Subagent terminology migration | Complete | The legacy parallel-executor term is removed; team contracts use `ManagerSubagent`, `TeamRole::Subagent`, and `TeamSpec.subagents` |
| Team Subagent usage aggregation | Complete | Generic Agent usage snapshots aggregate member token/cache/call totals into `SubagentResult` |
| Skill upstream check and sync | Complete | File-backed source records, local-change protection, atomic replacement, and CLI/TUI/GUI/channel parity |
| PDF/DOCX systematic-review rendering | Complete | Pandoc/Quarto discovery with selectable PDF engine and portable-format fallback |
| Real-provider and LSP smoke fixtures | Complete | Explicit ignored tests gated by credentials/environment variables |
| Legacy history placeholder IPC removal | Complete | Duplicate history commands/types removed; conversation export remains canonical |
| Formal plan materialization count contract | Complete | `docs/2026-07-19-formal-plan-materialization.md`; `task_execute` rejects inline/empty/partial plans and executes only the persisted PlanTask DAG |
| Formal plan execution identity and timeout reliability | Complete | Long-running dispatch tools own their deadline; `task_create` preserves the originating conversation/message identity so GUI Subagent cards and TaskRuntime use the same run |
| Parallel Subagent instance and TaskRuntime routing | Complete | Sync/Fork/Teammate dispatches use fresh factory instances; Auto/Task delegation is forced through the formal plan so the right panel cannot be bypassed |
| Revisioned dynamic plan runtime | Complete | `docs/2026-07-21-dynamic-plan-runtime.md`; atomic DAG creation, optimistic patches, split projections, safe-point reloads, and capability-scoped Subagents |
| Unattended worktree lifecycle and review parity | Complete | `docs/2026-07-22-unattended-worktree-lifecycle.md`; application commit `61c8350` |
| Logical-task worktree reuse and content-aware cleanup | Complete | `docs/2026-07-25-logical-task-worktree-reuse.md`; stable `{run_id}:{task_id}` isolation identity with attempt-scoped Subagent events |
| Unified Subagent prompt compilation | Complete | framework commit `8f7904f`; `echo-agent-app-core/src/subagent_prompt.rs`; one registration-time system prompt and one typed invocation compiler across direct, planned, fork, teammate, and team dispatch |
| Memory and self-evolution seam closure | Complete | `docs/2026-07-23-memory-self-evolution-closure.md`; replaceable workspace/hot-memory projections, one layered EKO write path, workspace-bound Curator, shared review integration, and stable compression dedup keys |
| Subagent result projection and attempt identity | Complete | `docs/2026-07-17-subagent-results-and-completion.md`; full terminal output is separated from process metadata and persisted for review/recovery, TaskRuntime snapshots auto-poll to authoritative plan/task state, the right rail separates execution from acceptance, and formal-plan `subagent_run_id` is `{run_id}:{task_id}:{plan_revision}:{attempt}` |
| GUI tool execution lazy loading | Complete | `docs/2026-07-25-gui-tool-execution-lazy-loading.md`; framework commit `27bb5a4`; application commit `d8b2211`; selector stability hotfix `b8c9077`; one main/Subagent summary path, opaque `detail_ref`, 64 KiB cursor pages, file/JSONL recovery, and complete Subagent prompt/result views |
| Runtime DAG kernel convergence and dispatch correctness | Complete | `docs/2026-07-27-runtime-dag-kernel-convergence.md`; one framework DAG loop/validator, atomic revision-safe claims, superseded-attempt rejection, revision-scoped durable results, lossless persisted status detail, and EKO-owned product resource limits |
| Task tools framework migration (task_create/update/list) | Complete | framework commit `38da658`; application commit `64da422`; `docs/2026-07-28-task-tools-framework-migration-design.md`; deprecated `todo_write` removed; modern revisioned TaskCreate/Update/List model migrated to framework behind `RevisionedTaskStore` trait; EKO's `TaskRuntimeStore` implements the trait and owns product persistence/bootstrap |
| App-core full migration audit + Iteration 0 dead-code cleanup | Complete | `docs/2026-07-28-app-core-full-audit.md`; deleted `sensitive.rs` (zero callers), `embedded_server.rs` + `server_pid.rs` (self-referential dead pair), and `config.rs` (5-line re-export shim, callers expanded to `echo_agent::config::...`); full audit of ~50 app-core modules confirmed only 3 real framework gaps remain (storage file impls S1/S2/S3); webhook/HITL/config_watcher confirmed as app-layer product policy with local bug-fix iterations, not migrations |
| Iteration 1: instruction system + turn/run identity | Complete | `.eko/AGENTS.md` is migrated to the unambiguous `.eko/learned-rules.md`; `InstructionProvider` is the sole EKO protocol owner and composes `echo_core::InstructionResolver::agents_files_only()` for the standard root-to-cwd `AGENTS.md` / `AGENTS.override.md` chain without scanning `.echo-agent/*` or `CLAUDE.md`; project-root discovery is shared and VCS-root-first, while `.eko/local.md` is resolved from the actual working directory; instruction and hot-memory content use distinct replaceable projections; Chat/Auto events keep `run_id=None` until a real TaskRun exists, while Task mode value-carries its pre-created run id. |
| Iteration 2: webhook + HITL + config_watcher fixes | Complete | The dead webhook singleton was removed. One `Arc<WebhookEmitter>` is shared by chat, scheduler, and the active surface; lifecycle emission now lives in the common `drive_chat` path, giving GUI/TUI/CLI/channel the same `ToolCalled`/`ToolFailed`/`AgentError`/`ChatCompleted` behavior, while cron emits `CronTaskCompleted`. Config reload watches the parent directory, accepts create/modify/remove and atomic-save events, uses resettable debounce, and hot-reloads both hooks and webhook endpoints; deleting global/project `hooks.yaml` removes its live registrations. Model/MCP/runtime topology still requires restart. HITL snapshots providers before await, broadcasts concurrently under one shared deadline, and drops remaining futures after the first response. |
| Iteration 3: migrate 3 file-backed storage impls down to framework | Complete | `FileRuntimeStateStore`, `FileConversationStore`, and `restore_message(s)` are framework capabilities; EKO uses them without enabling SQLite. File writes use unique temp names, file fsync, atomic rename, Unix parent-directory fsync, cleanup on failure, path-safe ids, and explicit corrupt-JSON errors. `FileConversationStore` serializes complete single-process read/modify/write operations, atomically implements `ensure_conversation`, reconciles stale counters from records on reopen, and normalizes stored message ownership. Message projection/restore now round-trips canonical roles, tool identity, multimodal content, and reasoning metadata. Conversation search stays on the canonical `ConversationStore::search_conversations` authority; EKO retains only path ownership and UI projections. |
| Tool Schema budget and recoverable output Phase 0-6 | Complete | Framework commits `9fad29f`, `bbca516`; `docs/2026-07-29-tool-schema-budget-and-artifacts.md`; one framework registry, invocation-local Tool Search, query-and-result-bound cursor pagination, recoverable SQL/Web/task artifacts, and content-free metrics; current Schema gates are Chat 3,647 / Task 3,906 / Auto 3,929 estimated tokens |
| Analytics runtime and EKO Polars convergence | Complete | `docs/2026-08-18-analytics-runtime-convergence.md`; framework commit `614b1cf`; existing framework `run_code` accepts an application-owned resolved script profile; EKO owns one locked Python analytics environment and no longer enables optional Rust Polars; measured built-in catalog reduced from 90 to 70 while first-turn schemas remain 15/16/18; locked runtime probe and real OS-sandbox execution pass |
| Canonical edit and real image tools | Complete | Framework commit `957f9e9`; `docs/2026-08-19-canonical-edit-image-tools.md`; one transactional `apply_patch` replaces seven default mutation entries, `view_image` sends validated local pixels only to image-capable models, and browser frames use the same redacted rich-result path. Combined with analytics runtime convergence, the registered built-in catalog target is 64. |
| Process-level shared PluginRuntimeService (P0-4) | Complete | `echo-agent-app-core/src/plugin_runtime.rs`; GUI/TUI/CLI share one serialized runtime owner. Bootstrap and every mutation stage a complete candidate, then replace plugin-owned components; failed wiring or lifecycle activation restores the previous live set and callbacks. Plugin config values and active theme/output-style preferences persist atomically. Host applications may explicitly register native callbacks; once registered, rewire is bracketed by deactivate/activate and uninstall unregisters them. EKO's declarative package loader does not currently register native callbacks. |
| Plugin component runtime wiring (P1) | Complete | Root `plugin.json` and fixed flat component locations are loaded and exactly unloaded. GUI/TUI/CLI expose the same live catalogs and output-style selection. GUI and TUI immediately synchronize plugin Theme activation and fallback after reload/disable/uninstall; built-in GUI selection deactivates the plugin preference. Theme and output-style preferences survive restart. |
| Flat plugin package convergence | Complete | Old `.echo-plugin/manifest.yaml`, `.mcp.json`, namespaces, component path declarations, and duplicate discovery rules are removed from the authoritative path. The framework owns manifest/Skills/MCP/Subagent/Hook/LSP semantics; EKO converts root `monitors.yaml`, `themes/`, and `output-styles/`. Standard component errors isolate at the smallest practical boundary. |
| Hook execution closure | Complete | The main registry exposes 31 emitted events and 7 Action types. `subagent` and `mcp_tool` actions are wired automatically to live runtime executors; plugin command hooks receive portable root/data environments; stderr and timeout/spawn failures are surfaced. Failed tool results emit `PostToolUseFailure`; the canonical call-scoped `permission_mode_override` participates in approval without mutating global mode; all Hook sources share strict registration filtering. CLI/TUI/GUI Hook tests use the same matcher-aware dry-run, and the watcher reloads app config plus global/project `hooks.yaml` on create/modify/remove. Evolution writes, layer changes, candidate detection, and health checks emit their declared events. Plain HTTP supports loopback/private/link-local IPs, localhost, single-label hosts, `.local`, and `.lan`; remote hosts require HTTPS. User-configured MCP tools have no deny-list. |
| Typed TaskRuntime lifecycle and hook delivery | Complete | `TodoStatus` and `RuntimeEventKind` preserve cancelled/timed-out terminal states through store, executor, Hook bridges, GUI/TUI/CLI/channel projections, and generated TypeScript. `HookEventDispatcher` uses a bounded ordered queue with producer backpressure plus explicit flush/idempotent shutdown. |
| Dynamic Provider/model protocol convergence | Complete | Users own a dynamic Provider registry and any number of models per Provider. Every model explicitly selects Chat Completions, Responses, or Anthropic plus text/image/audio/video input capabilities. A single framework `ThinkingProfile` registry resolves verified model-specific levels from provider endpoint + API protocol + model id; GLM starts at 5.2, Claude at 4.6, unknown models remain usable with auto only, and GUI/TUI/CLI consume the same choices. Framework endpoint resolution is provider-neutral, while all surfaces share the AppState mutation and connection-test paths without a second provider or thinking mapping. |
| MCP Resource model surface | Complete | `docs/2026-08-19-mcp-resource-tool-surface.md`; the existing MCP manager now projects three canonical, read-only list/template/read tools from connected resource-capable clients. EKO keeps them searchable but out of every mode's first-turn schema. |
| Foreground turn ownership convergence | Complete | `echo-agent-app-core/src/foreground_turn.rs` is the EKO authority for exact `(surface, conversation, turn)` admission, cancellation, supervised driver settlement, and ordered generation receipts. GUI, CLI REPL, channel, and TUI now use it end to end; each surface retains only transport and renderer projection state. |
| Background command cells + awaiter role (Phase C1-C4) | Complete | Design: `docs/2026-08-14-eko-long-horizon-task-runtime-design.md` §11 (C track). Framework commits `58e6733`, `7f66ff5`: one `CommandCellRegistry`, sandbox-preserving launch, durable output artifacts, retry-safe multi-waiter cursors, explicit owner cancellation, and UTF-8-safe bounded output. Application commit `5cf49c2`: process-wide registry, low-thinking `awaiter`, `watch_cell`, TaskRuntime start/finish events, recovery-capsule projection, active-cell completion blocker, explicit-cancel propagation, and boot recovery that closes orphaned cells without replaying external commands. Pause keeps cells alive; only explicit run cancellation stops them. |
| Long-horizon TaskRun continuation control plane (Phase C5) | Complete | One app-layer `TaskContinuationRuntime` owns idle continuation without a second graph/executor/store. Finite RunTurns have event-folded claim, exact driver settlement, token/time budgets, compaction accounting, Goal Contract/Recovery Capsule, blocker audit, cell wakeup, durable provider retry, typed boot admission, stable surface HITL replay, versioned Requirement/Evidence, cross-surface controls, and discardable checkpoint projections. The automated fault matrix and the accepted 12-hour real soak are complete; detailed evidence is governed by `docs/2026-08-17-eko-long-horizon-runtime-m5-evaluation.md`. |
| Long-horizon runtime M0-M5 implementation | Complete | `docs/2026-08-16-eko-long-horizon-runtime-implementation-plan.md`; Runtime Goal complete. App `de09946` makes `TaskRun.goal` the revision/hash-bound authority. Framework `cd4fccf` and app `9d59a0b` close M1. Framework `6d7d0cf` plus app `f4771f3` close M2 with exact-attempt controls through the existing `TurnSteerMailbox`. App `aa92178` closes M3 with event-folded provider retry, typed boot admission, exact orphan recovery and safe staged run creation. App `54d8bc4` closes M4 through one store-owned Requirement/Evidence completion report shared by execution and GUI/TUI/CLI/channel. App `3e409d0` closes M5a with a schema/hash-validated discardable checkpoint, suffix-only event fold, crash-window snapshot repair and fixed release performance gates while preserving `events.jsonl` as sole authority. App `82d8eda` adds the resumable, commit-pinned long-horizon harness; its canonical provider/crash/disk/HITL/Subagent/cell/Goal-drift matrix is all green. The 12-hour ledger passed with 1,439 ended turns, 143 compactions, 11 production recoveries, zero failed turns and complete final hashes. On 2026-08-19 the user accepted 12 hours as the final real-soak gate and waived 24/48-hour completion; those services were stopped and their durable ledger snapshots retained without being represented as passes. |
| Cross-workspace Agent messaging and groups | Complete | Applications `00bf3d4`, `f3b6f2c`, `15d3ad3`, `e13a309`, `b97bb23`, `9ee2312`, `de867e3`; `docs/2026-08-20-cross-workspace-agent-groups.md`; independent loaded hosts and cross-host generations pass three-host gates. The application-owned `AgentRouter` discovers persisted addresses, owns durable inboxes and persistent Agent groups, and all interactive surfaces project the same service. A frozen PlanTask target acquires the exact remote conversation Agent while the leader TaskRuntime remains the sole DAG, retry, cancellation, review, and canonical SubagentRun authority. M8 adds transcript-owned completed-turn deduplication, stable correlated replies, concurrent three-inbox restart gates, and removes the remaining mutable-cwd transition naming without adding another executor/store/queue. |
| Local runtime recovery and surface settlement hardening | Complete | Framework `7417634` atomically replaces live memory/HITL tools and quarantines corrupt run logs. EKO keeps `events.jsonl` as the only TaskRuntime sequence authority under a cross-process lock, isolates unreadable run/tool projections without hiding healthy records, configures one explicit process data root, performs create-and-switch through one backend transition, and lets durable `turn_status` own GUI terminal settlement. |

## Current Decisions

### Cross-workspace Agent messaging and groups

The governing design is
`docs/2026-08-20-cross-workspace-agent-groups.md`. EKO owns global
workspace/session discovery, durable file inboxes, workspace runtime lifetime,
surface projection, and persistent Agent groups. The framework already owns
the required ReAct, steer, Task DAG, cancellation, revision, and Subagent
mechanisms and receives no new EKO routing model.

`AgentAddress` is the stable `(workspace_id, conversation_id)` identity.
Accepted delivery is persisted before wake, is at-least-once, and becomes
effectively once at transcript commit through message-id deduplication. Incoming
Agent content does not inherit user approval. Cross-workspace task execution
retains one leader-owned TaskRun and records remote work as its canonical
SubagentRun; do not add `GroupRun`, another DAG loop, another mailbox, a mirror
TaskRun, SQLite, or a permission gate.

M1 is complete: immutable workspace paths and file stores are centralized in
one `WorkspaceRuntimeResources` factory, and the existing workspace path uses
it. Application commit `00bf3d4` passed the full Rust submission gate on
2026-08-20. Application commit `f3b6f2c` closes M2: a process registry owns one
stable host per workspace identity, the focused workspace is a private host
reference, metadata refresh preserves immutable resources, and root drift is
rejected. M3 is complete: process-global cwd and pool/store rebinding are gone,
loaded hosts own their execution resources, all four live configuration
generations publish across loaded hosts, and three-host isolation/activity
gates pass. M4 adds one application-owned durable inbox with registry/store
address validation, restart replay, message-id deduplication, and unloaded-host
acceptance. M5 is complete: the application supervisor persists claim/settle
attempts, reclaims incomplete claims at boot, uses the existing exact-turn steer
mailbox for live targets, waits for foreground settlement when a target is busy,
and runs cold targets through the shared `drive_chat` path. Agent/runtime input
cannot approve HITL, correlated replies return through the same router, and no
second transcript writer, executor, or mailbox was introduced. The explicit
M5 transcript/receipt crash window is closed by M8 for completed transcript
turns: deterministic turn IDs, transcript markers, and stable reply IDs recover
without rerunning the model. Side effects before transcript completion remain
explicitly at-least-once. M6 projects one service across GUI/TUI/CLI/channels;
M7 adds persistent groups through the existing TaskRuntime target adapter; M8
also passes concurrent three-inbox restart/soak gates and removes the remaining
mutable-cwd transition naming.

### MCP Resources

MCP Resources remain a distinct contextual-data surface rather than being
converted into one tool per resource or folded into MCP server Tools. The generic
list/template/read adapters live in `echo-agent` and reuse its authoritative
`McpManager`; EKO owns only progressive schema exposure. A connected
resource-capable server adds three searchable registered tools and zero
first-turn schemas across Chat, Task, and Auto. See
`docs/2026-08-19-mcp-resource-tool-surface.md`.

### Long-horizon runtime M0-M5

The governing implementation plan is
`docs/2026-08-16-eko-long-horizon-runtime-implementation-plan.md`. `TaskRun.goal`
is the sole Goal authority; Plan revisions bind its revision and hash and do not
persist a second Goal. The existing revisioned `task_create/task_update/task_list`
graph, framework DAG validator, app-layer continuation runtime, file event store,
completion blocker path, and `TurnSteerMailbox` remain authoritative. Do not add
parallel Goal/Plan tools, a second mailbox, another completion evaluator, or
SQLite.

Logical product gates are `R0 -> M0 -> M1 -> M2 -> M3 -> M4 -> M5`. Repository
delivery follows dependencies: land generic CommandCell and Subagent control
primitives in `echo-agent`, then application Goal/correctness/control/recovery/
evidence policy in `echo-agent-cli`, then checkpoint, performance, fault, and
soak work. Framework prerequisite commits do not close product M1/M2 early. M1
must be completely green before cold-start auto-resume can be enabled.

R0 has persisted the plan and this cross-session record. After the planning
turn, the user explicitly instructed implementation, and the exact Codex
Runtime Goal was created on 2026-08-16. Every resumed session must verify that
Goal, read the implementation plan, this file, both repositories' status, and
recent logs before continuing, then update the milestone ledger with commits,
commands, failures, and remaining work.

### Plugin and Hook production closure

The 2026-08-16 audit used OpenAI's
[plugin packaging](https://developers.openai.com/plugins/build/plugins) and
[Codex hooks](https://learn.chatgpt.com/docs/hooks) contracts as the primary
industry reference, alongside the Agent Plugins 1.0 package contract already
adopted by EKO. Codex establishes four useful production baselines: fixed
package identity, portable plugin root/data paths, observable command-hook exit
semantics, and install-time validation. EKO keeps those baselines while using
its existing flat `plugin.json` layout and one atomic runtime instead of adding
a parallel Codex-specific package loader.

The framework owns the cross-product mechanisms: Hook parsing/execution,
Subagent and MCP executor wiring, portable plugin environment values, and
bounded failure output. EKO owns the product adapter: transactional
install/enable/disable/uninstall, GUI/TUI/CLI catalogs, and validation of every
fixed package component before activation. `plugins validate` now parses Skill,
Hook, MCP, Subagent, LSP, monitor, theme, and output-style content instead of
only confirming paths.

Codex additionally provides public/repository marketplaces, UI metadata, and a
managed trust-review flow. Those are distribution/governance capabilities, not
runtime correctness. EKO remains directly installable from local paths and Git
and deliberately does not add a trust approval gate: it is a local personal
assistant whose user explicitly chooses extensions. A future marketplace can
be an application-layer catalog over the same installer without changing the
framework runtime.

### Data Analysis

The persistent contract is `analysis/<id>/manifest.json` plus a reviewable
`analysis.py` or `analysis.R`, `environment.json`, `result.json`, `outputs/`,
immutable `runs/`, and `latest-run.json`. The saved script is executed through
`run_code(script_path=...)`; inline duplicate code is not an acceptable run.

This follows the common pattern documented by OpenAI Data Analysis, Jupyter's
file-backed contents model, and Quarto execution: code and outputs remain
inspectable, rerunnable, and attributable. EKO does not add a second notebook
kernel, a statistical DSL, or framework-authored inference algorithms.

### Research And Medicine

Sources, claim-level evidence, protocols, screening decisions, RoB judgments,
GRADE outcomes, and PRISMA counts are ordinary workspace JSON records. The GUI
is a projection of those records; Agent/TUI/CLI/channel operations use the same
application service and `research_library` tool.

Successful arXiv, Semantic Scholar, PubMed, and ClinicalTrials.gov tool results
are now ingested before they return to the Agent. Direct OpenAlex, Crossref, and
Europe PMC searches use reusable framework clients and the same idempotent
`SourceRecord` merge path. Zotero Web API sync is explicit and never persists
the API key in research records.

Each systematic review can run a deterministic citation audit and export a
Markdown report, complete JSON package, evidence CSV, BibTeX library, and RIS
library. Europe PMC enrichment stores citation/reference identifiers and
biomedical entities in the source record and saves available full text as XML.

The medical extension follows PRISMA 2020, Cochrane RoB 2/ROBINS-I, and GRADE
as quality contracts rather than adding a separate runtime state machine.

### Coding

LSP discovery only starts installed language servers detected for the current
project; explicit global and project `.lsp.yaml` files override discovery. The
repo map uses Tree-sitter for supported languages and a UTF-8-safe text fallback
for unsupported files or parser failure.

Unattended execution no longer creates a duplicate run-level
`eko-unattended-*` checkout. Read-only primary-Agent work stays in the
authoritative checkout; mutation is forced through a formal writer PlanTask,
whose `eko-fork-*` worktree is keyed by `{run_id}:{task_id}` and reused across
attempts. Formal-plan attempt identity is
`{run_id}:{task_id}:{plan_revision}:{attempt}` for events and audit, while the worktree
isolation key remains `{run_id}:{task_id}`.
Finalization removes a checkout immediately when Git proves it has no
uncommitted files or unique commits; changed checkouts are unlocked and retained
for retry, review, or integration. Retained legacy branches are managed by one
app-core review path surfaced in both GUI and TUI. See
`docs/2026-07-22-unattended-worktree-lifecycle.md` and
`docs/2026-07-25-logical-task-worktree-reuse.md`.

### Formal Plan Execution

The canonical parallel path is one atomic `task_create(tasks=[...])`, followed
by `task_list` and `task_execute(revision=N)`. Later changes use one
optimistically locked `task_update(base_revision=N, operations=[...])`. The
runtime rejects inline tasks, empty plans, stale revisions, invalid DAGs, and
unknown Subagent/tool capabilities before dispatch. `agent_tool` remains the
single ad-hoc Subagent mechanism in Chat mode. Auto and Task mode physically
hide it, so delegated work materializes a formal plan and appears in the right
task panel. The TaskRun itself represents the user goal, so the plan contains
no wrapper task. See `docs/2026-07-21-dynamic-plan-runtime.md`.

Factory-backed Sync, Fork, and one-shot Teammate dispatches construct an
independent Agent per invocation. This is required because ReactAgent serializes
one instance for its entire execution lifetime; reusing the registry singleton
made concurrently submitted same-role Subagents queue behind one mutex. These
modes also propagate an invocation child cancellation token, including explicit
cancellation when their internal deadline expires. TeamAgent's persistent
member/mailbox lifecycle remains a separate path and retains its own identity
semantics.

### Subagent Prompt Compilation

The framework exposes one product-injectable `SubagentPromptCompiler` for both
registration-time system prompts and dispatch-time invocations. EKO compiles a
cache-stable system prompt from role Markdown, common orchestration rules,
frontmatter-derived capabilities, the optional follow-up policy, one language
anchor, a self-contained result-quality rule, and the canonical result contract.
The final answer and structured summary cannot refer to reasoning or content
"above" because the parent and each surface may consume terminal output without
the thinking trace. Role Markdown owns only identity and role-specific method.

Direct dispatch and TaskRuntime dispatch use the same compiler. TaskRuntime
passes a typed payload containing the goal, domain, task, dependencies, files,
checks, acceptance criteria, artifacts, boundary, and delegation facts. Fresh
context is the default; Fork may transfer only filtered structured history and
never embeds the parent system prompt as user text. The effective Subagent
catalog is an immutable snapshot derived from the same definitions used for
registration, including project and user roles, and startup validates every
default route against it.

### Dynamic Plan Runtime

The runtime accepts one atomic full-DAG `task_create` and revisioned
`task_update` operations. `events.jsonl` is the recovery authority, `plan.json`
is the latest immutable plan specification, and `run-state.json` is the
execution projection. The scheduler reloads revisions at safe points, completed
attempts are never restarted implicitly, and Subagents report suggestions but
never mutate the plan directly. See
`docs/2026-07-21-dynamic-plan-runtime.md`.

Long-running formal execution is not governed by the ordinary 120-second tool
deadline. `task_execute` and other timeout-exempt tools use their own bounded
execution policy, including the Subagent dispatch deadline, in both streaming
and non-streaming ReAct paths. The framework also carries the originating
conversation and `message_id` through
`ExternalRunContext -> AgentRunSnapshot -> ToolContext/SubagentEvent`.
When `task_create` lazily materializes a TaskRun in Auto mode, it persists that
conversation/message identity instead of substituting the internal
`taskrun:<turn>` id. The right task panel and the inline main-chat Subagent
stream therefore resolve the same formal run and remain visible while the plan
executes.

Subagent execution identity is attempt-scoped and specification-aware. `task_id`
identifies the stable PlanTask node;
`subagent_run_id = execution_id = {run_id}:{task_id}:{plan_revision}:{attempt}` identifies
one concrete formal-plan dispatch. Framework-dispatched Subagents use framework lifecycle
events, direct primary execution uses application Subagent events, and
TaskRuntime integration events use a separate task scope. The frontend stores
all attempts independently, keeps terminal state monotonic, and defaults to the
latest attempt when rendering a task. The result view uses complete terminal
output without its internal protocol envelope; it never promotes a thinking
trace into the result. The Subagent process view contains only the same lazy
tool-execution rows used by the primary Agent. File access remains result
metadata. Terminal records are retained
until explicit clearing, and TaskRuntime loads start polling automatically so a
completed backend snapshot cannot remain displayed as Pending after the live
trace disappears. TaskRuntime review consumes the complete output rather than
the bounded parent summary, and persists that output on the terminal boundary so
restart recovery receives identical evidence. File-backed Todo reads take
TaskExecution status from `run-state.json`; older Task events only restore
metadata and cannot override a later plan skip/reset. The right rail reports
Subagent execution progress separately from Task acceptance progress.

As of 2026-07-27, dependency traversal is no longer an EKO-owned `run_dag`
loop. `echo-orchestration::RuntimeDagExecutor` owns revision safe points, ready
frontiers, bounded Subagent waves, cancellation, failure propagation, external
in-flight polling, and stall detection. EKO injects file-backed snapshots,
review, resource/file ownership policy, worktree integration, and events through
`EkoRuntimeDagController`. The old 596-line application scheduling loop was
deleted. The framework runtime model now separates immutable spec from mutable
execution, preserves execution checks/acceptance/artifacts without flattening,
and uses the existing framework `PlanValidator` as the sole structural DAG
validator. EKO validates only its Subagent/tool catalog and file-ownership
policy. See `docs/2026-07-27-runtime-dag-kernel-convergence.md`.

Dispatch is protected by an atomic `TaskClaim` containing plan revision,
attempt, and TaskSpec hash. A claim conflict reloads the snapshot instead of
failing the task; completion/failure/block/retry writes are accepted only for
the still-running claim, so an old attempt cannot overwrite cancellation or a
new plan. `run-state.json` stores shared `TaskStatus` detail independently from
`failure_fingerprint`. The framework limits only concurrent Subagents; EKO's
write/shell/LLM limits remain application policy in `EkoExecutionLimits`.

### Skills And Report Rendering

Skill updates follow Claude Code's explicit marketplace refresh model: users
initiate the network operation, existing Git credentials are reused, and the
last good local copy remains available when an update fails. EKO adds a
file-backed source record and refuses to overwrite local edits unless the user
passes `--force`. Reference: <https://code.claude.com/docs/en/plugin-marketplaces>.

Systematic-review PDF/DOCX output delegates conversion to Pandoc (or Quarto,
which bundles the same document pipeline) instead of implementing layout in the
Agent framework. Reference: <https://pandoc.org/MANUAL.html>.

### Memory And Self-Evolution

Instruction files and hot memory remain ordinary local files, but their model
context is now a replaceable projection instead of a boot-only system-prompt
suffix. Bootstrap, workspace switch/exit, Dreaming promotion, explicit hot
memory mutation, and rule promotion refresh the primary Agent immediately;
pooled Agents refresh too, and future pooled Agents inherit the current working
directory. GUI, TUI, and CLI own and settle their Dreaming schedules today.
TUI normal Chat is supervised by the shared foreground owner and settles before
Dreaming, review, and workspace transition teardown. Standalone channel mode
still needs the same Dreaming schedule composition; that remaining adapter must
reuse the existing owner rather than introduce a second schedule authority.

EKO product writes use one `MemoryLayerManager` path and the unified
`agent/memories` namespace. Raw Store tools and the optional cold tier remain
framework capabilities for other consumers. Memory review stays proposal-only;
deterministic Dreaming maintenance does not authorize hidden semantic merges.
Compression-time heuristic and LLM extraction share one content-derived key so
the same fact is not persisted twice. See
`docs/2026-07-23-memory-self-evolution-closure.md`.

### 2026-07-19 Real-Environment Verification

- OpenAlex, Crossref, and Europe PMC live search passed, and the application
  persisted their results through the file-backed source library.
- An installed LSP server was discovered, initialized against a real
  multi-language project fixture, and shut down cleanly. Broken executable
  proxies are rejected by a bounded version probe.
- Pandoc 3.9.0.2 plus Typst 0.14.2 produced real systematic-review DOCX and PDF
  artifacts through the application export path.
- Zotero credential smoke tests are implemented and ignored by default. They
  were not executed because `ZOTERO_API_KEY` and `ZOTERO_LIBRARY_ID` were not
  present in the environment; no credentials are stored by EKO.

### 2026-07-28 Task Tools Framework Migration

- The framework now publishes `task_create`, `task_update`, and `task_list` as
  first-class tools backed by one `TaskRevisionService`, `TaskPatchEngine`,
  `PlanValidator`, and optimistic CAS protocol.
- Every ReAct Agent gets an instance-local in-memory task graph by default.
  The process-global `todo_write` implementation was deleted in the same
  framework change, so zero-configuration task tracking remains available
  without a second id/status model.
- EKO injects `EkoRevisionedTaskStore` and `EkoTaskToolPolicy`. These adapters
  own file/event persistence, run bootstrap, `DomainProfile`, Subagent/tool
  capability policy, attachments, and metadata round trips; they do not own
  patch semantics or DAG validation.
- Primary, pooled, GUI, TUI, CLI, and channel Agents replace the default
  in-memory store through the shared framework registration function. The
  Tauri `update_tasks` IPC command also calls `TaskRevisionService`.
- The old app-core `TaskCreateTool`, `TaskUpdateTool`, and `TaskListTool`, their
  schema/parsers, and the production `TaskRuntimeStore::update_tasks` API are
  gone. `TaskUpdateRequest` remains only as an EKO frontend wire DTO and is
  converted losslessly to the framework patch protocol.

### 2026-08-04 User Input Normalization (complete)

Long pastes and large text uploads previously reached the model fully inlined,
and stayed inlined across ReAct turns and session restore. The industry
converges on reference-then-search-then-read (Claude Code spills tool output
over ~50K chars; Codex writes long goals to `goal_files`). EKO adopts the same
mixed strategy: short text inlines, long text spills to a user-input artifact
and is delivered as a lightweight reference + preview, with
`grep`/`read_artifact` recovering the content on demand. Direct pastes are
tracked separately from uploaded data so a pasted request is not mislabeled as
untrusted log content.

**Layering:** application layer owns `PreparedUserTurn`, source classification,
threshold policy, spill/TTL cleanup, entry-point normalization, and the UI
persistence projection. The framework only resolves generic artifact roots in
`grep` and reuses `read_artifact`; EKO configures one common artifact root with
`user-input/` as an application-owned subtree. No second artifact reader, no
SQLite, and no new Task state were introduced.

**Authoritative input path:**

- `PreparedUserTurn` owns instruction + `InputResourceRef` resources and the
  single `to_message()` merge point. Raw messages spill at 32 KiB or the token
  budget; text resources explicitly sourced from paste always spill. Writes
  are atomic, previews are UTF-8 safe, and SHA-256/line/byte metadata travels
  with the reference.
- `drive_chat`/`drive_chat_inner` (`chat_driver.rs:197`) now take
  `&PreparedUserTurn` instead of `(&str, Option<&Message>)`; the
  `match multimodal` merge block is gone — `to_message()` is the single
  authoritative collapse point. Mode-hint folding moved into
  `PreparedUserTurn::build`.
- All six entry points switched: GUI send (`chat.rs`), GUI steer
  (`steer_chat_message`), TUI send (`events.rs` send path + `send_to_agent`),
  TUI steer (`/steer`), CLI REPL (`start_chat_with_agent`), channel. Each now builds
  a `PreparedUserTurn` via `UserTurnInput` + `resolve_user_input_spill_dir`.
- `ensure_task_mode_run`'s goal is now `turn.instruction` (reference block for
  spilled text = better task goal than the raw paste). Only inline resources
  enter `ChatResources.attachments`; tool-reference text is never re-inlined by
  TaskRun/subagent reconstruction.
- Dual implementation eliminated: the in-memory `build_message` is deleted;
  its 3 tests migrated to `build_message_from_refs` (which remains for the
  `executor.rs:2790/2948` subagent rebuild path).
- TUI and GUI stage long clipboard text as `source=paste`; short pastes remain
  editable text. Preparation failure restores the draft/resources and does not
  enter a processing state. Channels return a retryable error instead of
  falling back to full-text inline delivery.

**Artifact reachability and lifecycle:**

- `echo-tools/src/files/grep.rs` confinement accepts a candidate-root set
  (`base_dir` + `working_dir` + `ctx.output_artifacts.root_dir`). The model can
  grep spilled tool-output and user-input artifacts by the absolute paths
  `read_artifact` / `PreparedUserTurn` already hand out. Both candidate roots
  and the resolved path are canonicalized (mirrors `read_artifact`), so
  symlink/`..` escapes are caught. No `ToolContext`/schema/pipeline changes.
  Relative paths also resolve against the artifact root when it is the only
  configured root.
- `read_artifact` and `grep` share the configured `.eko/artifacts/` root, so
  both framework tool output and EKO's `user-input/` subtree are reachable.
- Conversation deletion removes both tool-output and user-input scopes; a
  best-effort 30-day TTL cleanup runs when new input artifacts are written.

**Persistence:** framework-projected `MessageContent::Parts`, tool calls, and
tool results remain authoritative. Frontend display metadata is merged by
user/final-assistant role order rather than raw array index, so tool messages
cannot shift the mapping. `display_content` keeps internal artifact references
out of history rendering, and pasted/oversized attachment data URLs are not
duplicated. GUI restore delegates to the framework's `restore_messages`, which
round-trips structured artifact references.

### Plugins (P0-4)

`PluginRuntimeService` (`echo-agent-app-core/src/plugin_runtime.rs`) is the single
process-level owner of the `PluginRegistry`. It holds an `AgentHandle` plus a
serialized runtime state, and `project_root` is derived from the
agent's `working_dir` so workspace switches are reflected without recreating the
service. It owns initial wiring during `AgentRuntime::bootstrap` and is surfaced
on `AppState.plugin_runtime` as well as the TUI and CLI command contexts.

All GUI/TUI/CLI plugin commands delegate to it. Each operation is serialized.
Reload parses and validates a complete candidate before mutating the live
runtime, validates and persists plugin config, and substitutes EchoAgent
variables before parsing fixed plugin components and plugin Skill
content. It starts required plugin LSP servers, replaces scheduled
monitors, and then swaps framework Skills/Hooks/MCP plus executable Subagent
factories. Optional host-provided lifecycle callbacks can be registered through
the explicit runtime API; once registered, the runtime deactivates active
callbacks before rewire and activates the candidate set only after successful
wiring. Any deactivation, wiring, or activation failure unloads partial
candidate wiring, restores the previous component set and callbacks, and leaves
the published registry unchanged. Uninstall performs deactivate/shutdown and
unregisters the callback. Runtime mutations acquire the
serialized plugin state before scheduler replacement, so binding and reload use
one lock order. Candidate LSP preparation treats every currently running base
server as required, preventing a plugin reload from stopping unrelated language
services. Themes are an application-owned renderer catalog; output styles are
replaceable system-context projections. GUI and TUI immediately synchronize
plugin Theme selection and fallback when reload/disable/uninstall removes it;
selecting a built-in GUI theme deactivates the plugin preference first. Both
theme/output-style choices survive process restart.

Workspace generation changes use an application-owned two-phase adapter. Plugin
and hook sources are fully preflighted before the process cwd commit boundary;
after that boundary the target workspace is always published, with a typed
`WorkspaceTransitionReceipt` reporting any subsystem that settled partially.
Foreground turns, TaskRuntime file operations, and pooled agent execution use
one ordered admission barrier; TaskRuntime callers receive a typed Busy result
during a transition rather than blocking an async executor thread.
The one owned config-watcher handle acknowledges each rebind, rebuilds target
hooks immediately, reports actual residual watch directories, and is cancelled
and awaited at shutdown. Plugin rebinds reuse the same registry, LSP manager,
and framework receipts. A shared MCP name-ownership registry gives canonical
user configuration priority over plugin declarations; exact plugin tokens make
old receipts incapable of disconnecting a user connection that later took over
the same name. Failed target plugin generations converge to the target's
User-scope plugins and retire the old Project/Local generation. If that fallback
also fails, the runtime publishes the target root after removing every
plugin-owned live receipt and reports User-scope plugins as degraded instead of
leaving any component bound to the previous workspace.

EKO has no production `register_lifecycle` callsite, so declarative packages do
not claim native callback support. Before adding a native plugin host, choose one
authority: process-scoped callbacks that explicitly retire the previous current
generation, or workspace-scoped callbacks keyed by scope + generation + exact
ownership token. Do not extend the current name-only manager into a second
lifecycle state machine.

The scaffold writes a root Agent Plugins 1.0 `plugin.json`, unique Subagent and
output-style names, and valid Skills, Hooks, MCP, LSP, monitor, theme, and
output-style files in the complete flat package layout. Strict validation uses
the framework resolver for reusable EchoAgent components and the application
parsers for every fixed EKO component format, including Hook action validation.
Real integration-style tests use an executable stdio JSON-RPC LSP fixture and a
live scheduler instead of count-only mocks.

The package contract follows Agent Plugins 1.0 official
[manifest](https://agent-plugins.org/plugin-authors/manifest),
[MCP](https://agent-plugins.org/plugin-authors/mcp-servers),
and [loading](https://agent-plugins.org/client-implementers/loading-and-discovery)
guidance. Cursor's root Agent Plugin support provides an independent client
reference; Cursor-specific `.cursor-plugin/plugin.json` remains a client-owned
format rather than EKO's canonical package layout. EKO keeps one portable root
manifest, one fixed location per supported component, and component-level
failure isolation. It deliberately has no plugin namespace layer because it is
a local personal assistant with one authoritative plugin runtime.

Framework/application placement is explicit: portable manifest validation,
fixed Skills/MCP, standard variables, failure isolation, scopes, and reusable
EchoAgent Subagent/Hook/LSP primitives belong to `echo-agent`. EKO monitor
policy, themes, output-style projections, GUI/TUI catalogs, and runtime
preferences remain in `echo-agent-app-core`. The application adapter only
discovers `monitors.yaml`, `themes/`, and `output-styles/` and converts them to existing product types;
it does not duplicate registry, dependency, Skill, MCP, or lifecycle logic.
Candidate preparation remains in EKO because Agent construction, monitor
policy, themes, and output-style projections are product concerns; generic
path resolution and reversible Subagent registration remain framework APIs.

### TaskRuntime hook delivery

The persisted `RuntimeEventKind` is also the typed event kind transported to
GUI/TUI/CLI/channel consumers. Cancellation and timeout are enum variants from
the original `ReactError`/`SubagentStatus`; no consumer classifies terminal
state by searching error or event text. The ordered Hook dispatcher uses a
bounded synchronous producer queue and a dedicated async consumer runtime.
Flush is a FIFO barrier; shutdown prevents new sends, drains prior events, and
is idempotent. This matches Tokio's documented
[bounded backpressure and clean-shutdown model](https://docs.rs/tokio/latest/tokio/sync/mpsc/index.html)
while accommodating the store's synchronous append hook.

### Foreground Turn Control

Foreground turn ownership is EKO product policy and remains in app-core. The
framework continues to own ReAct execution and same-turn steering; app-core
wraps the existing `drive_chat` and `TurnOutcome` instead of adding another
lifecycle state machine. The GUI path now acquires one exact
`(surface, conversation, turn)` lease, uses the lease's cancellation token,
keeps ownership until the driver settles, and exposes an exact active snapshot
for WebView remount recovery. A downstream sink rejection cancels that same
token; when no terminal event was accepted, the sole returned and registered
outcome is `Failed` with code `downstream_disconnect`, not a synthetic user
cancellation.

The old GUI `active_chat_turns` and chat token maps are deleted. The CLI REPL
now uses the same owner and routes concurrent ordinary input through framework
steering or one FIFO follow-up queue. After a steer rejection, the broker waits
on the tracked supervisor completion and HITL enqueue channels; it calls the
same synchronous Reedline reader only when a real HITL request is pending, and
also selects the process Ctrl-C signal while no line editor is active. Typed
settlement automatically starts the FIFO head exactly once. Because there is no
line prompt in that parked wait, `/exit` is accepted before entering it or from
a pending-HITL prompt; Ctrl-C remains the always-available exact-cancel path.
Non-empty
HITL request IDs are reserved until their exact pending request and requester
both release them, so a duplicate cannot enter the broker queue. Its sole stdin
owner remains Reedline's synchronous `read_line`: `/exit`, Ctrl-D, and Ctrl-C can request exact
cancellation and await typed settlement after Reedline returns a signal, but an
external future cancellation cannot interrupt an in-progress blocking read.
If the outer future is dropped after control returns, the tracked turn's drop
guard requests exact cancellation and aborts the sole supervisor; dropping that
future drops the lease and defensively settles the registry. Because Drop cannot
await, this boundary does not claim a join, but there is no inner or detached
chat task left running.
There is deliberately no per-request reader, `spawn_blocking` reader, detached
thread, or second stdin owner; Reedline's external printer is the only path for
turn/HITL output while line editing is active.

CLI startup registers the owned REPL HITL session before headless services can
emit requests, then moves that same session into the input broker. Bootstrap
failure, normal exit, and session failure unregister it and reject every exact
pending request. Its exact dispatcher receipt also unregisters synchronously
when the caller future is aborted; dropping an older receipt cannot remove a
new provider registered under the same surface name. If Reedline's
external-printer sink closes or fills, the HITL
provider rejects the triggering request immediately, closes further admission,
and publishes a session failure that cancels and drains the tracked foreground
turn. Idle chat input always enters the existing FIFO before admission, so a
head retained by `Busy` or `AdmissionSuspended` cannot be bypassed by a newer
line after admission reopens. The bootstrap `ReviewIntegration` is the sole
memory authority: each `ChatResources` carries that integration and lets the
shared driver acquire its generation after foreground admission; no legacy
layer-manager snapshot or temporary replacement integration remains. The CLI
Dreaming owner retains both cancellation and `JoinHandle`, joins before
auto-memory/session review, and reports a join failure through the REPL terminal
result.

Channel chat,
management commands, exact reset, steering, and stop now use the same owner;
framework reset aliases are disabled at `SessionHandler` composition so they
cannot replace a generation before EKO settlement. The channel driver is held
by the owner's `JoinSet`; accepted reset settlement uses the same owner even if
its transport waiter disappears. Its composite receipt stack defines
`Foreground -> TaskRuntime -> Memory -> pool` acquisition with reverse release
before the typed terminal is published. The shared chat driver acquires the
Memory generation after TaskRuntime registration and before pool admission,
passes that exact lease through `ChatResources.memory_generation`, and derives
the layer manager from it. This preserves one owner and prevents a second
manager snapshot or generation reacquisition.
Shutdown inserts the existing `ReviewIntegration` settlement immediately after
foreground shutdown and before workspace transition shutdown.

TUI now acquires the same exact lease and runs its driver through the owner's
supervisor. `active_turn_id` is renderer correlation only; `FinalAnswer`,
`Cancelled`, `Error`, and textual status events cannot release busy state or
advance the follow-up queue. Only an exact matching `TurnSettled` projection
does so once. Retryable admission failures restore the original FIFO head and
leave the editor draft and attachments untouched. Steering settlement races
reuse that FIFO instead of discarding accepted input. TUI HITL is one
request-id-reserving FIFO owner with atomic close-and-drain, and its exact
dispatcher registration is removed before the shared ordered shutdown drains
foreground, model, Dreaming/review, workspace, scheduler, TaskRun, pool,
plugin, watcher, MCP, browser, and Hook owners. The shared chat driver remains
the sole TaskRuntime/Memory/pool generation authority; TUI does not snapshot a
legacy layer manager or add a framework lifecycle API.

## Next Step

Cross-workspace messaging M0-M8 is complete in
`feature/cross-workspace-agent-groups`. Before integration, merge the latest
application `main` into the worktree branch, preserve the relative framework
dependency paths, rerun the applicable full submission gates, and then merge
the application branch. The accepted architecture remains one leader TaskRun,
one canonical SubagentRun attempt identity, one AgentRouter, and the shared
`drive_chat` transcript writer.

### Completed long-horizon objective

The Codex Runtime Goal is complete with this exact objective:

```text
完整实现 EKO 长程任务运行时 M0-M5，包括 Goal 生命周期、正确性、
Subagent 控制、恢复、完成证据和性能评测。
```

R0/M0/M1/M2/M3/M4, M5a and M5b automation are complete. Application
`de09946`/`9d59a0b`/`f4771f3`/`aa92178`/`54d8bc4`/`3e409d0`/`82d8eda` and framework `cd4fccf`/`6d7d0cf`
passed their full workspace, no-default, lint, and applicable feature/GUI/frontend
gates. M3 persists provider retry attempts, deadlines and typed fingerprints,
pauses as `ProviderUnavailable` on durable limits, closes orphan execution facts,
and only auto-resumes `Paused/BootRecovery` unattended runs after one typed
admission verifies launcher, ownership, workspace, Goal/Plan, budgets and blockers.
M4 binds stable GoalRequirement IDs to Goal/Plan revisions, revalidates artifact
hashes, invalidates affected evidence after Goal changes, and projects the same
store-owned completion report to GUI/TUI/CLI/channel without a second state or
`goal_complete` tool. M5a keeps `events.jsonl` authoritative while a compact,
schema-versioned and hash-verified checkpoint retains the sole fold state and
deduplication keys; reads detect a durable suffix before trusting snapshots. The
automated M5b fault matrix is all green. Commit `61a3e389` launched concurrent,
isolated real 12/24/48-hour runs through user launchd on 2026-08-17. The 12-hour
run passed after 43,200,302 active milliseconds with 5,971 events, 1,439 ended
turns, 143 compactions, 11 recoveries, zero failed turns and no failure
fingerprint. On 2026-08-19 the user accepted that result as the final real-soak
gate and waived completion of the 24/48-hour runs; their services were stopped
and their unchanged ledger snapshots retained. Evidence is recorded in
`docs/2026-08-17-eko-long-horizon-runtime-m5-evaluation.md`
and the run index `docs/2026-08-17-eko-m5-soak-runs.md`.

Tool context optimization Phase 0-6 is closed in
`docs/2026-07-29-tool-schema-budget-and-artifacts.md`. Operational follow-up is
limited to live-model success-rate measurement using the content-free counters.
Do not add another tool registry, placeholder schemas, application-owned cursor
engine, eager Browser/MCP schema surface, or EKO telemetry database.

Runtime DAG convergence is complete: the framework owns canonical
`TaskSpec`/`TaskExecution`/`TaskStatus`, validation, traversal, and the generic
claim protocol; EKO owns checked file/UI projections, claim persistence, and
product policy. `TaskManager` cycle queries now delegate to the same canonical
dependency analysis as `PlanValidator`; the old manager-local DFS is gone. Do
not reintroduce another loop, validator, status model,
canonical task-spec name, or unclaimed dispatch path. Task relations are also
unified: all modes expose
`task_create/task_update/task_list/task_execute`; one Task and a dependency DAG
share the same revisioned TaskRun graph. The framework has replaced
`todo_write` with per-Agent revisioned task tools, while `TaskPlan` and `TodoItem` remain only
artifact/UI projections. Do not reintroduce `plan_*` CRUD tools or an
independent todo store.

The app-core full audit (`docs/2026-07-28-app-core-full-audit.md`) reviewed all
~50 app-core modules against the framework. Verdict: **only 3 real framework
gaps remain** — the file-backend impls of `RuntimeStateStore` (S1),
`ConversationStore` restore direction (S2), and `ConversationStore` file impl
(S3). These will migrate down with bug fixes (corrupt-JSON errors, path-safe
IDs, unique temp names, parent-dir sync, `Result`-returning restore). Everything
else stays in the app layer: webhook emitter, HITL dispatcher, and config
watcher are EKO product policy (delivery semantics, multi-surface arbitration,
reload scope) with no cross-product unified answer — they get local bug-fix
iterations, not framework migrations. Iteration 0 dead-code cleanup is complete
(`sensitive.rs`, `embedded_server.rs`+`server_pid.rs`, `config.rs` shim all
deleted). Next, observe real
long-running GUI/TUI/CLI task runs for claim-conflict and revision-conflict,
safe-point, logical-task worktree reuse, and clean-finalize telemetry. Review the nine retained
legacy `eko-unattended-*` branches through the new queue before explicitly
cleaning them. Also sample real direct, planned, fork, teammate, and team
Subagent runs through prompt diagnostics to confirm the expected section
cardinality and response-language behavior across providers. Treat any UX or
observability improvement discovered there as a new milestone; the revisioned
dynamic plan runtime, worktree repair, and unified prompt compiler are complete.
Also sample workspace switching and long-lived sessions to verify instruction
projection replacement, Dreaming's first post-boot pass, and hot-memory budget
growth before adding entry-level limits or a more frequent idle trigger.
Run a real GUI smoke test with a large streaming shell result: confirm the
collapsed row remains responsive, expanded output advances by cursor while
running, completed output loads only one page at a time, history reload restores
summaries without eager detail reads, and conversation deletion returns before
background detail cleanup finishes.

Add one cross-surface integration gate over GUI, TUI, CLI REPL, and channel that
covers independent scope, stale-id rejection, sink disconnect failure, and
settlement-before-release through their real transport adapters. Then continue
the broader capability-parity matrix for Task/Subagent/reviewer,
MCP/LSP/browser/terminal, and research/evolution/memory without introducing
surface-local lifecycle owners.
