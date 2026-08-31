# ADR 0027: Framework-Native Domain Values

- Status: accepted
- Scope: `echo-agent-cli/echo-agent-app-core` and Tauri permission commands

## Decision

EKO stores and passes framework-owned domain values directly. Permission state
uses `echo_agent::tools::permission::PermissionRule`; it does not define a
parallel `PermissionRuleConfig` or `PermissionBehavior`. Tauri only parses
transport strings, including `PermissionMode`, with the framework's `FromStr`
implementations and attaches the EKO description.

This keeps product policy and surface decoding in EKO while making the SDK
types the sole authority for generic permission semantics. The same standard
applies to delivery and Subagent outcomes: no source-named framework
conversion helpers or mirrored generic DTOs are retained during development.
`ConversationInputOutcome` is likewise only an EKO wire name; its Rust value is
the framework `echo_agent::agent::AgentSteerTurnOutcome` directly, with no
per-variant conversion.
`ChatSteerOutcome` is also only a GUI wire name and reuses the same framework
outcome directly in Rust.
`PluginInstallScope` is only the EKO command wire name for framework
`PluginScope`; CLI shorthand parsing uses the framework's standard `FromStr`
implementation (`scope_value.parse()`).
Model provider views likewise use framework `LlmApiProtocol` and
`ModelInputModality` directly; the old `*Wire` enums were only redundant
variant-for-variant conversions and are gone.
MCP server entries likewise use framework `McpServerEntry` directly under the
EKO command's stable `McpServerConfig` wire name; the top-level command
document remains EKO-owned because it enforces its request schema.
Agent delivery receipts use framework `JournalDurabilityStatus` directly; its
serde-tagged shape is now the canonical durability wire value instead of an
EKO enum copy.
The revisioned disabled-tool policy uses framework `ToolControlService` and
`ToolControlSnapshot` directly; EKO adds only registered-tool validation,
pool fan-out, and the `effective_enabled` UI receipt field.
EKO also stores framework `ExecutionUsage` directly; `SubagentRunUsage` is only
the generated TypeScript wire name. The task executor no longer defines a
temporary `TaskExecutionUsage` DTO; delegated Subagent and primary-Agent paths
both expose the same usage value through their direct `usage()` result API.
Chat, continuation, and TaskRuntime lifecycle paths likewise
pass the framework `TurnReceipt` directly; EKO no longer defines a
`ChatTurnOutcome`, and only the final surface applies display rounding or
truncation. Its durable Subagent command identity is
the framework `SubagentCommandIdentity` type directly; the EKO name is only a
public application alias for the same value.
The durable command phase likewise aliases framework `SubagentCommandPhase`;
EKO retains only its UI/status projection.

## Consequences

- Permission list responses use the framework's serde shape.
- Invalid matcher, behavior, or source values fail at the request boundary.
- Framework additions are preferred over app-side conversion helpers when a
  generic operation is missing.
