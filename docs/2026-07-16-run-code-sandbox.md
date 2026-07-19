# M5 `run_code` Sandbox Closure

Date: 2026-07-16

## Scope

M5 closes the production bare-execution path for the Agent-controlled `run_code` tool. It does not change interactive terminal permissions, MCP connection permissions, or the EKO run state machine.

## Industry References

- [Claude Code sandboxing](https://code.claude.com/docs/en/sandboxing): OS-level isolation is the normal execution boundary; unsandboxed commands are an explicit escape hatch, and strict deployments can fail when sandboxing is unavailable.
- [OpenAI Codex security](https://developers.openai.com/codex/security): sandbox policy is separated from approval policy, so filesystem/network isolation is enforced independently from user interaction decisions.
- [OpenAI Codex sandbox selection source](https://github.com/openai/codex/blob/main/codex-rs/core/src/tools/sandboxing.rs): execution explicitly models whether sandboxing is automatic, required, or forbidden; bypass is not an implicit fallback after a sandbox failure.

The shared pattern is fail-closed code execution with an explicit isolation contract. EKO follows that pattern for Agent-authored code while preserving direct user terminal access as a separate local capability.

## Ownership

Framework (`echo-agent`):

- `SandboxCommand.minimum_isolation` and policy enforcement.
- Sandbox availability selection and typed timeout/cancellation results.
- UTF-8-safe shared stdout/stderr output budget.
- Local, Docker, and Kubernetes execution cleanup behavior.
- `RunCodeTool` validation, resource limits, result metadata, and failure classification.

Application (`echo-agent-cli`):

- Select the EKO sandbox configuration at startup.
- Probe whether the local OS sandbox is actually available.
- Remove `run_code` from the main Agent and Writer Subagents when the capability is unavailable.
- Keep Readonly Subagents free of code/shell write capabilities.

## Contract

1. `RunCodeTool` never starts a process directly. Missing, unavailable, or process-only executors return `unavailable`.
2. Every `run_code` command requires at least `os-sandbox` isolation. A permissive manager policy cannot lower that per-command minimum.
3. Timeout is clamped to 1-300 seconds and is passed to both `SandboxCommand` and `ResourceLimits`.
4. Cancellation is propagated from `ToolContext` to the sandbox executor. It is classified as `cancelled`, never automatically retried, and includes a postcondition to inspect possible outputs.
5. Retained stdout and stderr share one byte budget. Truncation is UTF-8 safe, while total observed byte counts remain available in metadata.
6. Non-zero exit, timeout, cancellation, and sandbox startup failure remain distinct final tool results.
7. EKO startup does not fail merely because code execution is unavailable; it disables only `run_code` and logs a concise capability warning.

## Production Paths

| Path | Sandbox wiring | Unavailable behavior |
|---|---|---|
| Main Agent | `SandboxManager::local_sandbox()` injected by `ReactAgentBuilder` | Remove `run_code` |
| Writer Subagent | Reuses the same manager and capability probe | Remove `run_code` |
| Readonly Subagent | Readonly tool registry excludes `run_code` | No change |
| Framework consumer | Consumer injects any `SandboxExecutor` meeting the contract | Structured `unavailable` result |

Interactive terminal and shell UI actions are outside this table because they are user-controlled capabilities, not the Agent-only `run_code` primitive.

## Acceptance Coverage

- No-sandbox and process-only executors fail closed.
- Python/R language mapping, case normalization, and working directory propagation are covered.
- Timeout and cancellation categories remain distinct.
- Local cancellation drops the execution stream, kills the process group, and prevents delayed writes.
- Docker and Kubernetes timeout/cancellation paths clean up the specific container or Pod.
- Output limiting uses a shared UTF-8-safe budget.
- Trusted sandbox policy still honors a command-level minimum isolation requirement.
- EKO removes `run_code` when OS sandbox probing fails and Writer subagents inherit the configured manager.

## Deferred Work

- User-facing full-log artifacts remain the separate tool-log milestone; M5 only bounds the retained result and exposes byte/truncation metadata.
- Container/Kubernetes live integration tests require provisioned backends. Unit and contract tests cover selection, arguments, cleanup branches, and result mapping in the default local CI environment.
