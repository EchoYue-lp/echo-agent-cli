# MCP Resource Tool Surface

Date: 2026-08-19

## Outcome

EKO now makes resources from connected MCP servers reachable to the model through
three canonical tools:

- `list_mcp_resources`
- `list_mcp_resource_templates`
- `read_mcp_resource`

The implementation reuses the existing `McpManager -> McpClient` connection path.
It does not create a second connection registry, resource store, or application
adapter. A resource-capable connection causes the three tools to be installed; a
topology change replaces their immutable client snapshot; disconnecting the last
resource-capable server removes them.

## Industry Evidence

The decision follows two primary implementations:

1. The official MCP Resources specification defines resources as contextual data
   such as files, database schemas, and application-specific information. Its
   stable discovery/read surface is `resources/list`,
   `resources/templates/list`, and `resources/read`. The protocol leaves the
   interaction model to the host application.
   [MCP Resources specification](https://modelcontextprotocol.io/specification/2026-07-28/server/resources)
2. OpenAI Codex exposes those protocol operations to the model as the same three
   global tools implemented here, with an optional server filter for listing and
   an exact server plus URI for reading.
   [Codex tool specifications](https://github.com/openai/codex/blob/main/codex-rs/core/src/tools/handlers/mcp_resource_spec.rs)
   and
   [Codex MCP Resource handlers](https://github.com/openai/codex/tree/main/codex-rs/core/src/tools/handlers/mcp_resource)

The cross-system pattern is therefore: keep MCP Tools and MCP Resources separate,
use one connection manager, expose a small global discovery/read surface, and let
the host decide whether those schemas are eager or deferred.

## Pre-Implementation Audit

The repository already contained most protocol machinery:

- `McpClient` negotiated the Resources capability, fetched paginated resources,
  cached discovery results, and implemented `resources/read`.
- Resource and resource-template wire types already existed.
- `McpManager` already owned every live client used by MCP tools and Hook actions.
- EKO already used one invocation-local progressive tool catalog in Chat, Task,
  and Auto modes.

The missing part was runtime reachability: no registered `Tool` allowed the model
to list templates or discover/read Resources. This iteration extends the existing
path instead of adding another MCP facade.

## Layering Decision

| Concern | Owner | Reason |
|---|---|---|
| Protocol pagination, Resource types, read/list calls, model-callable adapters | `echo-agent` | Generic to any MCP-enabled Agent |
| MCP connection topology | Existing framework `McpManager` | Already authoritative; no duplicate registry |
| First-turn schema selection | `echo-agent-cli` | EKO product policy shared by every interaction surface |
| UI rendering of future explicit resource pickers | `echo-agent-cli` | Product-specific projection, not required for model reachability |

The adapter snapshot is not an authority. It contains cloned `Arc<McpClient>`
handles from `McpManager` and is replaced after every connect, reconnect, or
disconnect operation.

## Runtime Contract

- The three tools exist only while at least one connected server declares the
  Resources capability.
- List operations accept an optional exact server name. Omitting it queries all
  resource-capable servers concurrently and retains per-server errors without
  hiding successful results.
- Targeting one failing server returns a structured unavailable failure.
- Native MCP cursor pagination is followed for at most 100 pages. The merged
  model result is independently bounded to 50 entries per page with an opaque
  continuation cursor.
- `read_mcp_resource` returns structured MCP contents without flattening text or
  base64 blobs into a lossy custom format.
- All three tools are read-only.
- EKO keeps them out of Chat, Task, and Auto first-turn schemas. They remain
  eligible for `tool_search`, so registered catalog size increases by three only
  while relevant, and first-turn schema count increases by zero.

## Deliberate Non-Goals

- This iteration does not upgrade the negotiated MCP protocol from 2025-11-25
  to 2026-07-28. The three baseline operations are compatible, while the newer
  mandatory request metadata, cache semantics, subscriptions, and multi-round
  trip results require a separate protocol-wide change.
- It does not add a second `request_user_input` tool. EKO already has one
  framework Human-in-the-Loop path for approval, free-form input, and selection,
  with GUI, TUI, CLI, and channel providers.
- It does not expose one tool per MCP server. Three global tools avoid multiplying
  schema count by server count.
- It does not add SQLite or application persistence for Resources.

## Verification

Focused tests cover:

- canonical names and read-only risk classification;
- native multi-page Resources and Resource Template discovery;
- deterministic cross-server model pagination and malformed cursors;
- UTF-8 resource names and text;
- preservation of text and blob contents;
- unknown-server argument failures;
- zero tools when no resource-capable connection exists; and
- deferred first-turn exposure in EKO Chat, Task, and Auto modes.

The full repository gates are recorded in the implementation handoff after they
complete.
