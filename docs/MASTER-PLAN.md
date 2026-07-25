# EKO Master Plan

Last updated: 2026-07-25

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
| Formal plan materialization count contract | Complete | `docs/2026-07-19-formal-plan-materialization.md`; `plan_execute` rejects inline/empty/partial plans and executes only the persisted PlanTask DAG |
| Formal plan execution identity and timeout reliability | Complete | Long-running dispatch tools own their deadline; `plan_create` preserves the originating conversation/message identity so GUI Subagent cards and TaskRuntime use the same run |
| Parallel Subagent instance and TaskRuntime routing | Complete | Sync/Fork/Teammate dispatches use fresh factory instances; Auto/Task delegation is forced through the formal plan so the right panel cannot be bypassed |
| Revisioned dynamic plan runtime | Complete | `docs/2026-07-21-dynamic-plan-runtime.md`; atomic DAG creation, optimistic patches, split projections, safe-point reloads, and capability-scoped Subagents |
| Unattended worktree lifecycle and review parity | Complete | `docs/2026-07-22-unattended-worktree-lifecycle.md`; application commit `61c8350` |
| Logical-task worktree reuse and content-aware cleanup | Complete | `docs/2026-07-25-logical-task-worktree-reuse.md`; stable `{run_id}:{task_id}` isolation identity with attempt-scoped Subagent events |
| Unified Subagent prompt compilation | Complete | framework commit `8f7904f`; `echo-agent-app-core/src/subagent_prompt.rs`; one registration-time system prompt and one typed invocation compiler across direct, planned, fork, teammate, and team dispatch |
| Memory and self-evolution seam closure | Complete | `docs/2026-07-23-memory-self-evolution-closure.md`; replaceable workspace/hot-memory projections, one layered EKO write path, workspace-bound Curator, shared review integration, and stable compression dedup keys |
| Subagent result projection and attempt identity | Complete | `docs/2026-07-17-subagent-results-and-completion.md`; full terminal output is separated from process metadata and persisted for review/recovery, referential summaries are recovered from the final thinking segment, TaskRuntime snapshots auto-poll to authoritative plan/task state, the right rail separates execution from acceptance, and `subagent_run_id` remains `{task_id}:{attempt}` |

## Current Decisions

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
attempts. Attempt identity remains `{task_id}:{attempt}` for events and audit.
Finalization removes a checkout immediately when Git proves it has no
uncommitted files or unique commits; changed checkouts are unlocked and retained
for retry, review, or integration. Retained legacy branches are managed by one
app-core review path surfaced in both GUI and TUI. See
`docs/2026-07-22-unattended-worktree-lifecycle.md` and
`docs/2026-07-25-logical-task-worktree-reuse.md`.

### Formal Plan Execution

The canonical parallel path is one atomic `plan_create(tasks=[...])`, followed
by `task_list` and `plan_execute(plan_revision=N)`. Later changes use one
optimistically locked `plan_patch(base_revision=N, operations=[...])`. The
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

The runtime accepts one atomic full-DAG `plan_create` and revisioned
`plan_patch` operations. `events.jsonl` is the recovery authority, `plan.json`
is the latest immutable plan specification, and `run-state.json` is the
execution projection. The scheduler reloads revisions at safe points, completed
attempts are never restarted implicitly, and Subagents report suggestions but
never mutate the plan directly. See
`docs/2026-07-21-dynamic-plan-runtime.md`.

Long-running formal execution is not governed by the ordinary 120-second tool
deadline. `plan_execute` and other timeout-exempt tools use their own bounded
execution policy, including the Subagent dispatch deadline, in both streaming
and non-streaming ReAct paths. The framework also carries the originating
conversation and `message_id` through
`ExternalRunContext -> AgentRunSnapshot -> ToolContext/SubagentEvent`.
When `plan_create` lazily materializes a TaskRun in Auto mode, it persists that
conversation/message identity instead of substituting the internal
`taskrun:<turn>` id. The right task panel and the inline main-chat Subagent
stream therefore resolve the same formal run and remain visible while the plan
executes.

Subagent execution identity is attempt-scoped. `task_id` identifies the stable
PlanTask node; `subagent_run_id = execution_id = {task_id}:{attempt}` identifies
one concrete dispatch. Framework-dispatched Subagents use framework lifecycle
events, direct primary execution uses application Subagent events, and
TaskRuntime integration events use a separate task scope. The frontend stores
all attempts independently, keeps terminal state monotonic, and defaults to the
latest attempt when rendering a task. The result view uses complete terminal
output without its internal protocol envelope; a referential terminal answer
promotes the final substantial thinking block and removes that block from process
rendering. File access remains process metadata. Terminal records are retained
until explicit clearing, and TaskRuntime loads start polling automatically so a
completed backend snapshot cannot remain displayed as Pending after the live
trace disappears. TaskRuntime review consumes the complete output rather than
the bounded parent summary, and persists that output on the terminal boundary so
restart recovery receives identical evidence. File-backed Todo reads take
TaskExecution status from `run-state.json`; older Task events only restore
metadata and cannot override a later plan skip/reset. The right rail reports
Subagent execution progress separately from Task acceptance progress.

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
directory. GUI, TUI, and CLI run the same Dreaming schedule.

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

## Next Step

Observe real long-running GUI/TUI/CLI task runs for revision-conflict,
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
