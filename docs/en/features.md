# EKO Feature Reference

This page lists capabilities reachable from production surfaces. Framework
mechanisms remain documented in `echo-agent`; EKO owns workspace policy, file
projections, review/worktree behavior, and product presentation.

## Agent and Conversations

- Streaming conversation is shared by TUI, GUI, CLI/JSONL, and channels through
  `drive_chat` and typed `TurnOutcome`.
- `AgentPool` retains one Agent per conversation and does not evict busy
  conversations. Keyed execution admission is provided by framework
  `KeyedExecutionAdmission`.
- Foreground admission, steer, cancel, settlement, durable input, attachments,
  and summary/hybrid/sliding/adaptive compression use shared app-core services.
  The default recent window is token-based; manual focus/cancellation and
  protected TaskRuntime goal/recovery projections share the same product policy.
- Framework `FileConversationStore` is the conversation authority; EKO adds
  workspace binding and UI projection.
- Completed GUI replies provide Copy, Regenerate, and Fork conversation actions.
  Fork copies history through the selected reply's complete user turn, including
  canonical tool calls/results, and opens an idle ordinary conversation in the
  current workspace list with a `(branch)` title. The source stays in the list;
  later turns and hidden internal Agent deliveries are excluded. Edit/regenerate
  still fork before the selected user turn and resend it.
- The GUI Side Conversation layout copies the primary conversation's committed
  context into a one-level, child-local Agent. AgentGroup metadata retains the
  relationship, model, and unread marker, while Tauri remains the only surface
  adapter. TUI, CLI/JSONL, and channels keep their existing conversation and
  Subagent behavior and expose no Side Conversation-specific contract.

Agent collaboration is organized automatically through model tools and the
runtime. GUI provides no manual messaging, group administration or collaboration
management page. Sidechat and Forkchat provide explicit user participation;
existing conversation and task/Subagent views continue to present results.

Archived conversations are hidden from the active sidebar and managed from
Settings -> Project data -> Archives. Restore is reversible; permanent
deletion uses the existing aggregate conversation deletion path.

## Tasks and Subagents

The product model is `TaskRun -> PlanTask -> SubagentRun`. The task tools use a
single revisioned graph with atomic plan updates, claims, retry, cancellation,
and safe-point reload. Long-running tasks add Goal, RunTurn, budget, provider
retry, boot admission, and checkpoint-backed hot state. Direct, planned, fork,
teammate, team, and plugin Subagents use the same `EkoSubagentPromptCompiler`
and framework outcome contract. Registration-time prompts declare the concrete
tool surface and shared visibility policy; invocation messages add the effective
allowlist and workspace while preserving typed attachments. Inherited history
keeps only filtered user and final-assistant messages. TaskRuntime reuses
framework JSON framing for optional follow-up data.

Framework `TaskStatus` is execution authority; `TodoItem` is a read-only query
projection. EKO adds `task_execute`, file projection, workspace policy,
review, worktree, and surface control. See [runtime architecture](./architecture/runtime.md).

## Tools and Extensions

EKO integrates transactional file edits, workspace diff, analytics execution,
interactive terminal sessions, Browser/Chrome, workflow catalog, structured
extraction, MCP, LSP, Tool output projections, model-driven Agent collaboration controls,
Hooks/Webhooks, Plugins, and Skills.

`ExtensionControlService` is the EKO mutation admission for Skills, Plugins,
MCP, Hooks, LSP, and Browser. It delegates to specialist owners and does not
create a second registry, manager, or store. Skill enablement uses one atomic
flat policy and immediate typed reconciliation; captured workspace identity and
the same command authority are shared across GUI, TUI, CLI/JSONL, and channels.

## Professional Workbenches

- Data analysis stores scripts, data, charts, and reports as file-backed
  artifacts.
- Research supports paper libraries, scholarly search, Zotero, Europe PMC,
  citation audit, and export.
- Medical review supports PICO/PECO, screening, RoB, GRADE, PRISMA, and
  applicability risk.
- Scheduling uses the file-backed cron scheduler and shared surface controls;
  prompt text is passed unchanged to the canonical TaskRuntime driver.
- Memory and self-improvement use generation-bound layered memory, safe-point
  projection, `/reflect`, Review Inbox, Curator, and rule/Skill promotion.

## Command-Cell Observation

`watch_cell` starts a bounded deterministic framework watcher and returns a
durable EKO receipt immediately. The watcher retains the cell, drains typed
cursor output through the real terminal, and publishes one Ready fact for every
surface. It does not dispatch a Subagent or depend on model/provider output;
`interrupt_command_cell_watch` never stops the command itself.

## Configuration and Observability

Provider and model configuration supports Chat Completions, Responses, and
Anthropic protocols plus text/image/audio/video input. Typed projections cover
traces, usage, cache, context budget, Tool/Subagent execution, TaskRuntime
events, HITL, Browser, and workspace deletion. All EKO data is stored as files,
JSON, or JSONL; the application does not use SQLite.

Only capabilities with a real registered handler are exposed by a surface.
Remaining project status and release residuals are recorded in
`project-status.md`, not duplicated in this feature reference.

EKO no longer bundles Skills or owns a default active set, methodology
baseline, or built-in catalog. SkillsHub manages independently installed
Skills under `~/.eko/skills/`; the current project's `<project>/.eko/skills/`
is discovered by its project runtime; plugin Skills come from plugin
generations. `enabled-skills.json` records only user Skill enablement, and disabled
entries do not register progressive activation or IntentRouter candidates.

External `SKILL.md` files still use the framework's agentskills.io parser and
validator, while Hooks remain application/plugin configuration. Repository
`.agents/skills/` files guide development of EKO and are not product runtime
content. The current project's `AGENTS.md` chain enters the same instruction
projection. See [ADR 0041](./adr/0041-external-only-skills.md).
