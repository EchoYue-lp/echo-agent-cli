# Tool Schema Budget And Recoverable Output

Status: Phase 0-6 complete on 2026-07-30.

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
- GitHub GraphQL exposes `endCursor` and `hasNextPage` as the continuation
  contract instead of embedding an offset in model-visible results.
  <https://docs.github.com/en/graphql/guides/using-pagination-in-the-graphql-api>
- Elasticsearch requires `search_after` calls to preserve the query and sort,
  and recommends a stable tie-breaker to prevent duplicate or missing hits.
  <https://www.elastic.co/docs/reference/elasticsearch/rest-apis/paginate-search-results>
- OpenTelemetry metrics aggregate numeric time series and control cost by
  limiting attributes; EKO therefore records counters and sizes, not content.
  <https://opentelemetry.io/docs/specs/otel/metrics/data-model/>

EKO applies the same pattern to its local tool registry. This is a context
budget decision, not a permission gate: every eligible registered tool remains
reachable through `tool_search`, and one invocation can opt out of deferred
visibility by omitting `visible_tools`.

## Architecture Boundary

| Responsibility | Owner |
|---|---|
| Single tool registry, deterministic Schema statistics, invocation visibility, tool search, pagination, artifact reader, content-free counters | `echo-agent` |
| Chat/Task/Auto first-turn groups and rollout, bundled Skill allowlists, 4K result default, local mode metrics | `echo-agent-cli` |
| Boundary | EKO passes initial/disabled names through `AgentInvocationContext`; it does not copy schemas, tools, execution, or cursor state |

The authoritative framework paths are:

- `echo-execution/src/tools.rs`: `ToolManager`, `ToolSchemaStats`, and
  `ToolSearchTool`.
- `echo-core/src/tools/mod.rs`: invocation-local `ToolVisibilityState`.
- `src/agent/snapshot.rs`: the one effective policy composition point for
  disabled tools, plan mode, Skill allowlists, and activated schemas.
- `echo-tools/src/files/artifact.rs`: bounded UTF-8 artifact recovery.
- `echo-core/src/tools/pagination.rs`: the only collection cursor contract.
- `echo-execution/src/tools.rs`: content-free `ToolBudgetMetricsSnapshot`.

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
| Chat | 15 | 14,591 | 3,647 |
| Task | 16 | 15,624 | 3,906 |
| Auto | 18 | 15,716 | 3,929 |

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

### Phase 4: Snapshot-Bound Cursor Pagination

`PageRequest { limit, cursor }` and `PageInfo` are the only collection paging
types. The opaque cursor contains a version, the next offset, and a SHA-256 of
the complete query identity, page size, and ordered result snapshot. Reusing it
after changing a path, query, filter, provider, backend, limit, or underlying
result set returns an invalid-argument result instead of duplicate or skipped
records.
Every page reports `page.next_cursor`, `page.truncated`, `page.total_known`,
`page.total`, and `page.returned` in `ToolResult.metadata`.

The real `glob`, `grep`, `list_dir`, `repo_map`, `code_search`, `diff`,
`git_diff`, `git_blame`, `web_search`, `search_memory`, and `sql_query` paths
use the contract. Results are sorted before slicing where the backend does not
already define relevance order. Generic tests prove consecutive pages preserve
Chinese/emoji content without duplicates and reject query, limit, or result
snapshot changes.

### Phase 5: Bounded And Recoverable Results

- Unified diffs are paged by file/header plus hunk, not arbitrary bytes.
- Directory and repository maps page stable entry/node lines.
- SQL pages rows; when the configured artifact threshold is crossed, complete
  page JSON is stored first and the inline projection bounds column names and
  cells with UTF-8-safe previews.
- `web_fetch` keeps the 10 MiB response safety cap but no longer discards a
  successful response at `max_length`: the complete text becomes an artifact.
- Completed `task_execute` calls return status and Subagent count while their
  complete persisted summaries are available through `read_artifact`.
- `grep` is the exact text entry point; `code_search` is the symbol/structure
  entry point. Browser, MCP, and `web_fetch_enhanced` remain deferred catalog
  capabilities rather than default schemas.

EKO's central output stage remains authoritative for the 4K model-visible
ceiling. Cursor pages and artifacts preserve complete accepted results instead
of retaining an unrecoverable head/tail projection.

### Phase 6: Content-Free Metrics And Rollout

`ToolBudgetMetricsSnapshot` atomically aggregates Schema requests/bytes/tokens,
activated-tool observations, Tool Search matches/misses, selection failures,
visible result bytes, spill bytes, aggregate duration, artifact reads, and
pagination counts. Per-result local tracing adds only tool name, status, sizes,
duration, pagination state, and artifact hash. It stores no query, user text,
tool output, URL, connection string, or secret.

EKO emits the same numeric Schema baseline under the local `eko::tool_budget`
tracing target and uses one `rollout_for_mode` policy. Deterministic Chat, Task,
then Auto gates are all enabled and tested. The production fixture stays under
25 tools and 4K Schema tokens in every mode; live-model task success rate is an
operational rollout metric, not fabricated by the unit suite.

## Rollback Boundaries

- Set `AgentInvocationContext.visible_tools` to `None` for one run to disable
  deferred loading without changing registration or execution.
- Change one mode's groups in `tool_exposure.rs` without changing framework
  behavior.
- Disable artifact spill configuration to retain the framework's inline result
  behavior for another consumer.
- Revert one tool to a single complete result if its cursor ordering proves
  unstable; do not retain two pagination protocols.
- Disable one mode's `deferred_schemas` flag in `rollout_for_mode` while keeping
  the framework registry and other modes unchanged.

## Closeout

Phase 0-6 are implemented. Future work is operational measurement against live
model task sets; it must consume these content-free counters and must not add a
second registry, cursor engine, output artifact path, or EKO database.
