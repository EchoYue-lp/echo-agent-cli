# ADR 0019: Unified Application Services Composition

- Status: Accepted
- Date: 2026-08-29

## Context

`AgentRuntime::bootstrap` already gives every EKO surface the same Agent, model generation, HITL,
Plugin, MCP, Browser, memory and command-cell primitives. The next layer was still duplicated:
`main.rs` and `tauri/desktop.rs` separately created `AppState`, TaskRuntime, AgentPool, config watcher,
scheduler, boot reconciliation, Agent control recovery and background maintenance. The headless path
also assigned `state.connection.pool` directly, bypassing `AppState::set_pool` and therefore the
workspace-aware `TaskExecutionTargetResolver`. It did not start the MCP health owner, and it kept a
separate Dreaming cancellation wrapper.

This is an EKO application-composition problem, not a missing generic Agent-framework supervisor.
Task graphs, stores, agents and cancellation remain framework/runtime primitives; config save paths,
workspace recovery, UI projections, scheduler policy, MCP health cadence and Dreaming are EKO product
policy.

## Industry references

- OpenAI Codex's central CLI parses shared `CliConfigOverrides` / `ConfigOverrides`, then dispatches
  surface-specific entry points such as `codex_tui::run_main`, `codex_exec::run_main` and app-server
  handling from one root. This supports a shared core/configuration composition with thin TUI, exec
  and app-server launch adapters rather than independently assembled runtimes:
  <https://github.com/openai/codex/blob/main/codex-rs/cli/src/main.rs>.
- Claude Code exposes resume/continue across CLI sessions and treats parallel subagents/sessions as
  views over the same product capability model, while worktrees isolate filesystem activity rather
  than defining a second Agent core:
  <https://docs.anthropic.com/en/docs/claude-code/common-workflows> and
  <https://docs.anthropic.com/en/docs/claude-code/sub-agents>.
- EKO's local capability snapshots in ADR 0002 and ADR 0003 already record the same cross-surface
  session/task/subagent distinction. ADR 0004 established one application-side lifecycle owner and
  explicitly rejected moving EKO lifecycle policy into `echo-agent`.

The mature pattern is not that every surface has identical rendering. It is that configuration,
session/runtime authority and lifecycle composition are shared, while a surface owns only its input,
output and host bridge.

## Options

1. Keep separate GUI and headless builders and add parity checks. This leaves two ownership graphs and
   makes every new process resource a multi-file synchronization obligation.
2. Add a supervisor to `echo-agent`. This would move EKO scheduler, workspace, config and maintenance
   policy into the reusable framework.
3. Add one app-core `ApplicationServices::compose` and make GUI, TUI, CLI/JSONL, channels and the LH6
   harness call it, while keeping their rendering and bridge owners local.

## Decision

Adopt option 3.

`ApplicationServices::compose` is the only authority that builds an EKO application generation after
`AgentRuntime::bootstrap`. It owns:

- the immutable config save path and config watcher;
- `AppState`, its canonical TaskRuntime, task tools and Agent control tools;
- AgentPool creation, initial surface permission publication and `AppState::set_pool`, including
  execution-target resolver installation;
- scheduler/task-service startup, boot reconciliation, extension reconciliation and durable Agent
  delivery recovery;
- MCP health and Dreaming background owners on the application root cancellation token;
- the `ApplicationLifecycleOwner` used for bootstrap rollback and graceful shutdown.

Surfaces register interactive HITL transports before composition may recover attended work. After
composition they retain only input/output policy: TUI rendering, REPL/JSONL/channel dispatch, or the
Tauri window and event bridge. Surface-only concurrent owners join the canonical lifecycle through
`ApplicationServices::track_external_owner`.

The explicit `--config` source is passed to composition. The watcher source and save target are
resolved together before workspace changes can redirect a relative path. Dreaming and MCP health use
the same root cancellation token and are awaited by the same lifecycle receipt.

`AgentRuntime::into_app_state`, `HeadlessServices`, `HeadlessServiceResources` and
`HeadlessDreamingOwner` are removed instead of retained as compatibility paths.

## Consequences

- GUI, TUI, CLI/JSONL, channels and LH6 now exercise one task/pool/recovery/maintenance topology.
- A successful surface bootstrap implies the workspace-aware task execution resolver and MCP health
  owner are installed.
- Dreaming shutdown is graceful and reported in the application lifecycle receipt; no surface-local
  abort-on-drop owner remains.
- Adding another application process resource requires one composition change, while adding a new
  surface requires only a thin adapter.
- `echo-agent` receives no EKO-specific supervisor or product policy. Framework examples and the
  website are unaffected because no framework public API or external product contract changes.
