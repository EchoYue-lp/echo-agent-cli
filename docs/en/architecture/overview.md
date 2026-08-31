# EKO Architecture Overview

This page describes the current production architecture of `echo-agent-cli`.
The ReAct engine, tools, memory, MCP, LSP, and workflow mechanisms are generic
framework capabilities; their public contracts are documented in the sibling
`echo-agent` repository.

## Product Boundary

EKO is a local personal assistant running on the user's machine. GUI, TUI,
CLI/JSONL, and messaging channels are input and rendering adapters over one
Agent product, not separate product editions.

| Layer | Responsibility |
| --- | --- |
| `echo-agent` | ReAct, model protocols, tools, DAG, Subagent, memory/store traits, MCP/LSP, and workflows |
| `echo-agent-app-core` | EKO runtime, workspace and conversation identity, TaskRuntime file projections, AgentPool, HITL, Plugin, Browser, analysis and research policy |
| `src/cli` / `src/tui` | CLI, REPL and TUI input, commands and rendering |
| `src/tauri` / `web-frontend` | typed Tauri IPC, GUI projection and interaction |
| `src-tauri` | desktop process entry, windows and platform facilities |

Workspace identity, GUI projections, worktrees, review policy, resource budgets,
and deletion policy remain application concerns. Reusable task graphs, state
transitions, model protocols, and tool contracts remain in the framework.

## App-Core Facade

`echo-agent-app-core::api` is the supported import boundary for CLI, TUI,
Tauri, channel, examples, and integration tests. It is a re-export facade; it
does not introduce a second runtime, store, DAG traversal, retry loop,
terminal reducer, or publication registry.

The physical modules keep authority-oriented boundaries. `state` owns
configuration, workspace/delivery scope, and `AppState`. TaskRuntime store and
executor modules own EKO file projections and adapters around framework
`RuntimeTaskService`/`RuntimeDagController`. Router, chat log, pool, extension,
plugin runtime, and infra modules follow the same inbox, journal, admission,
policy, publication, and factory boundaries.

AgentPool admission is backed by framework
`echo_agent::agent::admission::KeyedExecutionAdmission`. The framework owns
opaque-key leases, per-key process permits, retirement fences, close, and idle
waits. EKO retains Agent creation, capacity classes, cache eviction, workspace
generation, ToolControl, Plugin/MCP/model publication, and product receipt
mapping.

The split preserves JSON/JSONL, serde/TS bindings, file layout, error codes,
and five-surface behavior. A product-neutral framework primitive is admitted by
standalone semantics, dependency direction, and independent tests, examples,
and documentation; it does not wait for a second consumer. A separate EKO
contracts/domain/runtime crate remains a packaging decision that requires
dependency isolation, compile measurements, and multiple EKO consumers. See
[ADR 0025](../adr/0025-app-core-global-modularization.md) and the
[framework capability placement audit](../../../../docs/2026-08-30-framework-capability-placement-audit.md).

## Runtime Shape

```text
GUI / TUI / CLI / JSONL / Channel
                  |
                  v
       echo-agent-app-core services
  AppState / AgentRuntime / drive_chat / TaskRuntime
                  |
                  v
       echo-agent framework primitives
 ReactAgent / tools / stores / DAG / Subagent / MCP
```

The application owns product identity, file facts, workspace policy, review
and worktree policy, and surface projections. The framework owns generic
execution, task graph, receipt, tool, store, and protocol primitives.

## Data Flows

All surfaces eventually use `drive_chat`/`drive_chat_turn`:

```text
input
  -> PreparedUserTurn
  -> ForegroundTurnControl admission
  -> workspace runtime snapshot
  -> AgentPool conversation Agent
  -> framework streaming execution
  -> ChatSink typed events
  -> transcript/checkpoint/tool projection
  -> TurnOutcome settlement
```

The framework `TurnReceipt` remains the single authority for terminal outcome,
provider usage, compaction count, final message identity, sequence, and exact
elapsed time across chat, continuation, and TaskRuntime lifecycle. EKO does not
define a `ChatTurnOutcome` wrapper; Task and webhook surfaces read only the
fields they need at their final product boundary.

Task execution follows the single product model:

```text
TaskRun -> PlanTask -> SubagentRun
```

`PlanRevision` is an editable, versioned artifact. Framework `TaskStatus` is
the execution authority and `TodoItem` is a read-only query projection. EKO
adds file projections, workspace policy, review, worktree, and surface control
around framework task APIs.

See [runtime](./runtime.md), [persistence](./persistence.md),
[features](../features.md), and the relevant ADRs for detailed contracts.
