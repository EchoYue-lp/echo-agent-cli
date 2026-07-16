# M4 Tool Error Classification and Recovery

Date: 2026-07-16

## Scope

M4 standardizes failure facts across shell, file, search, Browser, and MCP tools. It does not add run states, a second executor, or a second diagnostics store. The existing `ToolManager`, `AgentEvent`, trace store, TaskRuntime durable boundaries, and GUI/TUI/CLI projections remain authoritative.

## Industry References

- [MCP tool specification](https://modelcontextprotocol.io/specification/2025-06-18/server/tools): protocol errors and tool execution errors are distinct; execution failures remain tool results (`isError: true`), clients validate results, apply timeouts, and log tool usage.
- [Temporal Activity definition](https://docs.temporal.io/activity-definition): an operation may execute more than once even when completion is observed once. Writes should be idempotent, use stable idempotency keys where possible, and verify external state before replaying an operation that may have partially completed.
- [AWS Builders' Library: Timeouts, retries, and backoff with jitter](https://aws.amazon.com/builders-library/timeouts-retries-and-backoff-with-jitter/): retries must be bounded, limited to transient failures, and delayed with backoff and jitter to avoid synchronized retry amplification.

The common pattern is: preserve a structured failure fact, retry only an explicitly safe subset, and treat uncertain side effects as a verification problem rather than a retry problem.

## Ownership

Framework (`echo-agent`):

- Generic failure categories, side-effect state, recovery action, retry metadata, idempotency key, and postcondition.
- ToolManager retry decision and backoff.
- Agent events, trace records, and MCP adapter mapping.
- Built-in shell, file, and search classifications.

Application (`echo-agent-cli`):

- Browser-specific classification and reconnect behavior.
- TaskRuntime persistence and recovery projection.
- GUI/TUI/CLI rendering.
- Cross-run concise diagnostics through the existing `TraceAnalyzer`; no new database or metrics product.

## Contract

Failure categories are stable and serialized as:

- `invalid_arguments`: the caller must change arguments; repeating the same call is ineffective.
- `unavailable`: a dependency, server, session, or capability is unavailable; restore or degrade before retrying.
- `timeout`: the deadline expired; retry only when side effects are known absent or an idempotency key makes replay safe.
- `cancelled`: user/runtime cancellation; never retry automatically.
- `transient`: short-lived failure; bounded automatic retry is allowed only when explicitly declared safe.
- `permanent`: retrying the same operation is not expected to help.
- `partial_side_effect`: the operation may have changed external state; verify the postcondition before deciding whether to continue.

Recovery actions are `correct_arguments`, `retry`, `restore_then_retry`, `verify_then_retry`, and `stop`. `retry_after_ms` is advisory. `idempotency_key` and `postcondition` are durable facts, not UI-only hints.

## Retry Rules

1. Automatic retry requires `recovery = retry`.
2. Automatic retry is forbidden for `invalid_arguments`, `cancelled`, `permanent`, and `partial_side_effect`.
3. Possible or confirmed side effects require an idempotency key; otherwise the action becomes `verify_then_retry`.
4. Streaming execution stops retrying after the first output chunk.
5. Backoff is exponential, bounded, and jittered. The configured retry count remains the hard cap.
6. Timeout and cancellation remain distinct terminal facts.

## Tool Mapping

- Shell: parse/safety failures are invalid or permanent; non-zero exit is permanent with possible side effects; timeout is `timeout` with possible side effects and a process-state postcondition.
- File: missing/conflicting paths are invalid arguments; writes expose path/content postconditions and stable call-derived idempotency keys where replay is safe; uncertain I/O after a write begins is a partial side effect.
- Search: empty queries are invalid arguments; provider/network failures are transient and safe to retry because search is read-only. The old private retry loop is removed so retries are visible and governed centrally.
- MCP: JSON-RPC/transport failures remain protocol failures and are classified from the framework error; `isError: true` remains an unsuccessful ToolResult. Read-only annotations permit bounded retries, while destructive/unknown tools require explicit idempotency or verification.
- Browser: lost sidecar/session is unavailable and may reconnect; cancellation never retries; locator/argument failures require correction; consequential actions with uncertain completion require page-state verification.

## Acceptance Tests

- Classification is identical in streaming terminal events, final AgentEvent, trace, TaskRuntime event, and GUI/TUI/CLI projection.
- Invalid arguments do not retry; transient read failures retry with a bound; streaming output disables retry.
- Shell timeout and Browser/MCP uncertain completion do not replay blindly.
- Stable `call_id`, idempotency key, and postcondition survive TaskRuntime persistence.
- Repeated failures across multiple runs use the existing trace reliability report and produce a short diagnosis without storing raw arguments.
