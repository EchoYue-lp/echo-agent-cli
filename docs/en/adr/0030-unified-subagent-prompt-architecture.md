# ADR 0030: Unified Subagent Prompt Architecture

## Status

Accepted

## Context

EKO previously had several partially converged prompt paths. Built-in roles used
`EkoSubagentPromptCompiler`, plugin roles could use raw Markdown and the
framework default compiler, TaskRuntime carried common task facts inside an
opaque JSON payload, team history had a separate relay, and the application
parsed optional fenced JSON itself. Listing tool names in the system prompt did
not prove that descriptions, disabled tools, or the concrete tool surface were
truthful.

EKO needs product-specific language, task, file-boundary, and follow-up policy,
but the compiler and structured-message execution mechanism are framework
capabilities. This boundary follows framework ADR 0024.

## Decision

1. One `EkoSubagentPromptCompiler` compiles built-in roles, plugin roles, direct
   dispatch, planned tasks, fork, teammate, team, and primary TaskRuntime work.
2. Stable system prompts contain only role/method knowledge, a
   `ToolCapabilitySnapshot` built after concrete tool registration, typed
   read/write/isolation/delegation boundaries, language policy, and the
   framework Result Contract.
3. The snapshot records tool name and bounded description plus visible and
   disabled sets. Built-in roles regenerate stable prompts on disabled-policy
   publication; plugin factories share the parent's framework
   `ToolVisibilityPolicy`, so future instances observe the same authority.
4. `SubagentTaskContext` owns dynamic user goal, task title, workspace, files,
   executable checks, semantic acceptance criteria, artifacts, and constraints.
   These facts no longer live in the opaque EKO payload. The remaining payload
   contains only DomainProfile, dependency summaries, and product task-boundary
   policy.
5. `CompiledSubagentInvocation.messages` is the exact execution input. The
   compiler owns the current typed message while preserving attachments and
   removes an immediately duplicated current user turn. Structured history
   remains real user/assistant messages; parent system prompts, tool traffic,
   reasoning, and runtime projections are never rendered as text.
6. Invocation-specific tool allowlists are combined with the concrete Agent's
   registered definitions and shared disabled policy after its effective
   workspace is known. `SubagentInvocation.capability_override` is separate
   from task context and is emitted only when an allowlist narrows the stable
   surface, so ordinary invocations do not duplicate the registered catalog.
7. The primary Agent has a stable TaskRuntime system profile compiled after its
   tools are registered. `compile_primary_invocation` generates only dynamic
   messages. Begin/end markers preserve unrelated prompt sections whenever a
   runtime prompt or methodology baseline changes.
8. The application Subagent catalogue is derived only from EKO definitions.
   The lossy `from_registered` tag decoder is deleted.
9. Optional suggested-task fields reuse framework JSON framing. EKO only
   validates and normalizes the product-specific task values.
10. Plugin conversion preserves framework `SubagentDefinition.access_mode`
    directly. Tool-control publication never reconstructs access from tags.

## Consequences

- Prompt claims can be compared directly with the concrete registered tool
  surface.
- Adding a dispatch mode or role cannot create another prompt builder.
- System prompts remain stable and cacheable; workspace and task facts remain
  invocation-scoped.
- Plugin and team paths follow the same language, boundary, and outcome policy
  as built-in Subagents.
- Role Markdown contains only identity, method, and domain knowledge.
