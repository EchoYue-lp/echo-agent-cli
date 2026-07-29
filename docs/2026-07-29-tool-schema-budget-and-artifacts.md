# Tool Schema Budget And Recoverable Output

Status: Phase 0-3 complete on 2026-07-29. Phase 4-6 remain.

## Decision Basis

The implementation follows three converging production patterns:

- Claude Code keeps large MCP tool definitions out of the default context and
  discovers them through tool search when the catalog grows.
  <https://code.claude.com/docs/en/mcp#scale-with-mcp-tool-search>
- Codex Skills use progressive disclosure: compact metadata first, complete
  instructions and resources only after activation.
  <https://learn.chatgpt.com/docs/build-skills>
- Cursor separates search from file retrieval so discovery does not require
  eagerly transferring complete content.
  <https://cursor.com/cn/docs/agent/overview>

EKO applies the same pattern to its local tool registry. This is a context
budget decision, not a permission gate: every eligible registered tool remains
reachable through `tool_search`, and one invocation can opt out of deferred
visibility by omitting `visible_tools`.

## Architecture Boundary

| Responsibility | Owner |
|---|---|
| Single tool registry, deterministic Schema statistics, invocation visibility, tool search, artifact reader | `echo-agent` |
| Chat/Task/Auto first-turn groups, bundled Skill allowlists, 4K result default | `echo-agent-cli` |
| Boundary | EKO passes initial/disabled names through `AgentInvocationContext`; it does not copy schemas, tools, execution, or cursor state |

The authoritative framework paths are:

- `echo-execution/src/tools.rs`: `ToolManager`, `ToolSchemaStats`, and
  `ToolSearchTool`.
- `echo-core/src/tools/mod.rs`: invocation-local `ToolVisibilityState`.
- `src/agent/snapshot.rs`: the one effective policy composition point for
  disabled tools, plan mode, Skill allowlists, and activated schemas.
- `echo-tools/src/files/artifact.rs`: bounded UTF-8 artifact recovery.

The authoritative application policy is
`echo-agent-app-core/src/tool_exposure.rs`. Chat, Task, Auto, GUI, TUI, CLI,
and channels reach it through the shared chat/task drivers.

## Completed Contracts

### Phase 0: Budget Baseline

`ToolManager::schema_stats_for` sorts definitions before serializing and
reports tool count, UTF-8 Schema bytes, and heuristic tokens. EKO's production
fixture loads bundled Skills and registers the application task tools before
measuring each mode.

| Mode | First-turn schemas | Schema bytes | Estimated tokens |
|---|---:|---:|---:|
| Chat | 15 | 14,246 | 3,561 |
| Task | 16 | 15,283 | 3,820 |
| Auto | 18 | 15,623 | 3,905 |

CI contracts reject more than 25 initial tools, 16,000 Schema bytes, 4,000
Schema tokens, or 4,000 tokens in one EKO tool result.

### Phase 1: Skill Names And Artifacts

Bundled `allowed-tools` entries now use real registered names such as `shell`,
`read_file`, `write_file`, and `git_*`. A discovery test loads every bundled
Skill and rejects matchers that cannot match a registered tool.

`read_artifact` reads spilled tool output by opaque byte cursor. Each page is
UTF-8 safe and bounded to 3,500 content tokens, returns `next_cursor`,
`truncated`, `total_bytes`, and `sha256`, and rejects changed/deleted artifacts
or a symlink escaping the configured artifact root. Tests recover a one-MiB
single-line JSON value containing Chinese and emoji without omission.

Tool results that exceed the EKO token budget spill even when they are below
the byte threshold. The model receives a short preview plus the exact artifact
path and full SHA-256 instead of an unrecoverable head/tail truncation.

### Phase 2: Product Exposure Groups

The EKO policy composes control, file, execution, task, Skill resource, Web,
repository, and memory groups per mode. Browser, MCP, extended domain tools,
Skill activation, and other non-first-turn capabilities remain searchable.
All modes keep `task_create`, `task_update`, `task_list`, and `task_execute` in
their first-turn Task graph surface.

### Phase 3: Deferred Schema Activation

`tool_search` searches lightweight name/description metadata and promotes up
to ten matching full schemas for the next model turn. Exact names, capability
queries, Skill allowlists, and EKO groups all mutate the same invocation-local
visibility object. There are no placeholder tools and no second registry.

Skill activation promotes tools matching that Skill's real allowlist during
the same invocation. Framework control tools (`final_answer`, `tool_search`,
Skill resource/script access, activation, and HITL) remain reachable when a
Skill narrows domain tools. Without a deferred surface, `tool_search` stays
hidden and the framework preserves its complete-schema behavior.

## Rollback Boundaries

- Set `AgentInvocationContext.visible_tools` to `None` for one run to disable
  deferred loading without changing registration or execution.
- Change one mode's groups in `tool_exposure.rs` without changing framework
  behavior.
- Disable artifact spill configuration to retain the framework's inline result
  behavior for another consumer.

## Remaining Work

Phase 4 defines one query-fingerprinted `PageRequest` / `PageInfo` contract and
migrates collection tools without retaining parallel cursor protocols. Phase 5
applies that contract and the 4K result budget to SQL, diff, directory, repo
map, Web, memory, and task execution output. Phase 6 adds content-free local
telemetry and rolls the policy through fixed Chat, Task, and Auto task sets.
