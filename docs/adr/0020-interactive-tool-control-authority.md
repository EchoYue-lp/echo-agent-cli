# ADR 0020: Interactive Tool Control Authority

## Context

EKO exposed GUI `enable_tool` and `disable_tool` commands, but they updated an
empty `SessionState.tool_states` map. No code populated the map and no Agent
execution path read it. The GUI then applied an optimistic local update, so a
toggle appeared to work until the panel reloaded.

The framework already owns the product-neutral mechanism. `ReactAgent::set_disabled_tools`
sets defaults for subsequent run snapshots; `ToolRuntime` removes those tools
from the model schema, and the execution pipeline rejects a provider-forced
call before its handler runs. Invocation-specific Task/Subagent exclusions are
merged with that default and remain independently authoritative.

This decision reuses the checked-in [Codex capability catalog](./0002-codex-tool-capability-catalog.md)
and [Claude Code capability catalog](./0003-claude-code-capability-catalog.md).
Both keep capability selection distinct from approval/sandbox policy: whether
a tool is available is not the same decision as whether an automated call
needs approval. For EKO, direct-user configuration is also not gated by
`permission_mode`; this is a local user configuring their own assistant.

## Options

1. Keep the session map and teach every invocation path to read it. Rejected:
   it would preserve a second execution authority and require GUI, TUI, CLI,
   channel, TaskRuntime, and Subagent paths to duplicate merging logic.
2. Unregister tools from the shared `ToolManager`. Rejected: registry mutation
   would affect in-flight and unrelated Agent generations, and would bypass the
   framework's immutable run-snapshot contract.
3. Own only the EKO user policy in app-core and project it through the existing
   framework disabled-tools API. Selected.

## Decision

- `ToolControlService` is the sole EKO authority for the disabled-name set and
  its monotonic generation. Mutations return a typed `ToolControlReceipt` that
  distinguishes the user's policy choice from effective framework availability.
- The seed `AgentPool` and every workspace fork share that service. Each pool
  projects the current generation into its primary, cached Agents, EKO
  built-in/formal Subagents, and their factories; future conversation Agents
  and freshly created formal Subagents read it during construction.
- Runs already holding a framework snapshot are unchanged. The next run sees
  the new generation, matching the framework contract.
- `AppState` validates that a requested name exists in the caller's current
  tool catalog, publishes the generation to loaded pools, and derives
  `ToolInfo.enabled` from the effective framework snapshot plus this policy.
- Tauri, GUI, TUI, CLI, and channel commands are adapters over the app-core
  authority. No surface owns a local enabled/disabled map.
- Tool availability remains separate from permissions and approval. This
  feature adds no `full-auto`, `default`, or other permission-mode gate.

## Consequences

- The phantom `ToolState`, `session.tool_states`, and `need_approval` tool DTO
  field are removed.
- Unknown tool names fail closed with a typed not-found response.
- Dynamic tools can be disabled by name once visible in the selected runtime;
  disabling does not unregister them or destroy MCP/plugin ownership.
- Plugin lifecycle and plugin-defined Subagent construction remain owned by the
  plugin generation boundary and are not mutated by this slice.
- GUI reloads, textual surfaces, existing conversation Agents, future
  conversation Agents, and workspace forks observe one policy generation.

## Verification

- Framework tests continue to prove schema filtering, snapshot immutability,
  invocation merging, and pre-handler rejection.
- App-core tests prove monotonic/idempotent receipts and propagation to primary,
  existing, future, and workspace-fork Agents.
- Surface tests and frontend contracts prove all adapters consume the typed
  receipt and effective `ToolInfo.enabled` projection.
