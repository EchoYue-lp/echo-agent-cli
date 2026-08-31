# ADR 0018: Typed Tool Artifact Projection

- Status: Accepted
- Date: 2026-08-29

## Context

The framework owns canonical `ToolInvocation`, `ToolResult`, stream events, and
the complete-output `ToolOutputArtifactRef`. EKO owns workspace storage roots,
retention policy, durable GUI detail, and surface rendering. Previously the EKO
repository and terminal surfaces reconstructed framework artifact descriptors
from fixed `ToolResult.metadata` keys. CLI and TUI also used local path
existence as a second availability inference.

Framework ADR 0011 moved the complete descriptor to typed
`ToolResult.artifact`. EKO must consume that fact without moving its product
storage policy into the framework.

## Considered Options

1. Preserve the metadata decoder in EKO. This leaves two accepted artifact
   contracts and allows replay or surfaces to disagree.
2. Move EKO workspace roots, retention, and detail pagination into the
   framework. These are product policies and would couple the reusable crate to
   the desktop application.
3. Persist the typed framework result losslessly, then apply EKO validation only
   when an artifact is exposed or read.

## Decision

Adopt option 3.

`ToolExecutionRepository` persists the canonical typed `ToolResult`. Its
verified reader checks the descriptor against registered EKO artifact roots,
retention, stable file identity, size, and digest. GUI detail pagination remains
application-owned.

CLI, TUI, JSONL/channel, and Tauri projections use `ToolResult.artifact`; they do
not parse artifact metadata or call `Path::is_file` to create a competing
terminal fact. Channel rendering exposes a reference only after the repository
returns the same verified descriptor. Tauri tool-detail commands remain thin
repository adapters.

## Consequences

- All EKO surfaces render the same framework artifact identity.
- EKO retains local workspace, retention, cleanup, and UI ownership.
- A missing or invalid artifact is omitted or returned as a typed repository
  error; surfaces do not silently reconstruct a partial reference.
- Tool enable/disable state is unrelated and intentionally outside this ADR.

## References

- Framework ADR 0011: `echo-agent/docs/adr/0011-typed-tool-output-artifact.md`
- MCP typed tool result content:
  <https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/schema/2025-06-18/schema.ts>
- OpenAI Agents Python typed tool output item:
  <https://github.com/openai/openai-agents-python/blob/main/src/agents/items.py>
- OpenAI Codex typed protocol events:
  <https://github.com/openai/codex/blob/main/codex-rs/protocol/src/protocol.rs>
