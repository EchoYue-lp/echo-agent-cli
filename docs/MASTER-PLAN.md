# EKO Master Plan

Last updated: 2026-07-20

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

### Formal Plan Execution

The canonical parallel path is now `plan_create` once per PlanTask, followed by
`task_list` and one `plan_execute(expected_task_count=N)`. The runtime rejects
inline tasks, empty plans, and count mismatches before dispatch. `agent_tool`
remains the single ad-hoc Subagent mechanism in Chat mode. Auto and Task mode
physically hide it, so any delegated work must materialize a formal plan and
therefore appears in the right task panel. The TaskRun itself represents the
user goal, so a formal plan contains no extra wrapper/placeholder task.
Main-chat tool rows and the right task panel therefore project the same
persisted PlanTask set instead of hidden one-task Runs. See
`docs/2026-07-19-formal-plan-materialization.md`.

Factory-backed Sync, Fork, and one-shot Teammate dispatches construct an
independent Agent per invocation. This is required because ReactAgent serializes
one instance for its entire execution lifetime; reusing the registry singleton
made concurrently submitted same-role Subagents queue behind one mutex. These
modes also propagate an invocation child cancellation token, including explicit
cancellation when their internal deadline expires. TeamAgent's persistent
member/mailbox lifecycle remains a separate path and retains its own identity
semantics.

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

### Skills And Report Rendering

Skill updates follow Claude Code's explicit marketplace refresh model: users
initiate the network operation, existing Git credentials are reused, and the
last good local copy remains available when an update fails. EKO adds a
file-backed source record and refuses to overwrite local edits unless the user
passes `--force`. Reference: <https://code.claude.com/docs/en/plugin-marketplaces>.

Systematic-review PDF/DOCX output delegates conversion to Pandoc (or Quarto,
which bundles the same document pipeline) instead of implementing layout in the
Agent framework. Reference: <https://pandoc.org/MANUAL.html>.

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

The requested coding, analysis, academic research, medical research, Subagent
terminology, Skill sync, document rendering, and smoke-fixture implementation
scope is closed. The only environment-dependent check left is running the
Zotero smoke test with user-supplied credentials.
Future work should start as a new milestone backed by a concrete user workflow
or measured reliability gap rather than another parallel runtime abstraction.
