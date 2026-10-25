# ADR 0054: Task-bound domain plugins

## Status

Accepted direction, implementation in progress. Source-only work has not yet been compiled or exercised.

## Context

Compiled professional profiles select planning instructions, default Subagents and review checklists independently of the live plugin generation. A TaskRun records the profile name but cannot explain which plugin version or rules governed it. Reloading a package must not silently replace the obligations of existing work.

The existing PluginRuntimeService/AgentPool publication and TaskRun journal already own the required lifetimes and state. The framework provides same-prepare plugin content identities and dependency selection. These are inputs to EKO policy, not a reason to add another executor, store or plugin manager.

## Alternatives

- Bind immutable effective domain rules at original TaskRun admission and check their source at each actual execution boundary (selected).
- Retain complete old plugin resources across reloads. A PreparedPluginSet Arc does not own old MCP connections, Hook registrations or package files; this would require a separate multiversion lifetime facility.
- Recompute existing work from the current package. This changes its meaning and is rejected.

## Decision

An optional package-root `domain.yaml` declares schema version 1, method, default Subagent roles, allowed tools, required artifacts, execution checks, acceptance criteria, review checklist and explicit package-relative inputs. Manifest name and optional version remain the sole package identity. EKO prepares and publishes the declarations with the existing component generation.

RunCreated and its original journal own the complete DomainBinding. TaskRun and graph metadata carry digest references; checkpoint and IPC are projections. Unknown or malformed binding data must not silently become an unbound General run. New explicit domain work uses the existing minimum Orchestrated TaskRun: AllowDirect only for work without formal obligations, otherwise RequirePlan and the existing minimum task graph. General's original no-plan conversational completion remains unchanged. The fixed direct Summary contract is not widened.

Effective identity includes the selected package/dependency closure, EKO monitors and output-style definitions, declared inputs and the actual active output-style provider/instructions. An effective style outside the declared domain closure is an explicit additional contribution. Pure theme colors remain presentation. Plugin role providers must be proven through their real Plugin source inside the domain's declared closure; a matching role name is insufficient.

The existing execution lease/publication boundary keeps identity checks and the actual invocation on the same generation. Original cleanup receipts retain physical MCP/Hook/Agent obligations. Domain/tool scope applies only to Agent automatic work, never user-operated terminal or MCP configuration. Empty `allowed_tools` retains the existing unrestricted meaning; an empty intersection of actual restrictions is rejected rather than encoded as unrestricted. Existing depth 1..3/default 1 and task revision/claim rules remain.

## Consequences

Professional method consumers move to the bound rule snapshot and compiled professional templates exit. General host methods and reasonable reusable capabilities remain. Package updates, disabling or missing original content can visibly block new work; they do not cancel a cleanup owner or reinterpret old evidence. The journal stores rules and identity, not credentials, live resources or a historical package archive.

Meaningful regressions must cover typed round-trips, both direct paths, dynamic obligation coverage, actual role/style sources, unrelated-package controls, reload/queued execution and recovery. Code, focused/full tests, native/platform evidence and exact main CI remain separate qualifications. Current source-only work claims none of those passes.

## References

- Stage Design `eko-domain-plugin-runtime/design.md`, accepted technical route A and Plan `plan_01_task-bound-domain-execution.md`.
- [Temporal version pinning](https://github.com/temporalio/documentation/blob/013b6f8b13f0613056250b17a5034960b3a440ae/docs/production-deployment/worker-deployments/worker-versioning/roll-out-and-pin.mdx): fixed execution routing does not imply checkpoint-retained code.
- [LangGraph compatibility](https://github.com/langchain-ai/docs/blob/4506df5fb4105a39b84c2b7390d21bed89e28668/src/oss/langgraph/backward-compatibility.mdx): state persistence alone does not fix the business meaning of resumed code.

## Implementation checkpoint boundary

The first source contribution switches real create_complex_task and GUI queued
input admission, both RunCreated journal forms, typed task graph/revision
projections, planned dispatch, Verification, review and fixed direct Summary.
The four compiled professional templates and enum-only visibility/routing exit.
General host behavior and the original execution/cleanup owners remain.

The source continuation connects adhoc agent_tool background/fork/teammate
through the original pool's physical retention and framework same-registration
admission. The existing TaskRuntime projector supplies persisted rules even when
agent_tool has no structured prompt payload. GUI preparation observes its already
owned lease without issuing another counted token; the weak control carrier is
dropped before settlement and only actual invocation resources retain admission.
The separate framework source must still be integrated and this EKO source has
not been compiled/tested or accepted as a completed Outcome.

The local example is included in an app-core regression, so later ordinary Rust
tests execute its parsing/input contract. The website work is owned by a separate
coordinated contribution; this application source slice does not change marketing
or claim release readiness. Generated TypeScript must come from the existing
export chain during the assigned Rust batch, not hand-written output.

## Registration and physical lifetime clarification

The accepted same-identity execution boundary is now concrete: the framework's
original registry resolves and retains one definition/factory/instance record.
Its fallible, product-neutral admission checks that exact record and the original
DispatchRequest; the original dispatcher consumes the same record. EKO interprets
the persisted domain binding, provider closure and definition digest. An earlier
name lookup, observation hook or current-catalog wrapper is not that authority.

EKO's original pool lease retains one physical keyed/process admission. Opaque
invocation projections retain that same admission across framework spawns; they
do not acquire another slot or retain a mutable Option cell. Original supervised
release waits for physical projections before performing its existing cache
retirement. Cancellation observations and unknown execution keep the original
receipt/debt authority. The original TaskRuntime projection registration travels
with the invocation only as needed to keep the same RunCreated rules available;
no additional store or dispatch loop is introduced. Before exposing an invocation
resource, the original fallible application registration captures its immutable
RunCreated binding/method projection. Model-boundary projection consumes that
same token-scoped data even if later journal/operation reads fail; the framework's
observational projector error path is not an execution gate. General retains
None-marker cleanup.

This continues the same Outcome under stage Design c0506ab8 and Plan f4d68e63,
Root checkpoint 5b6b7df. The framework API exists in a separate source-only tree;
its integration, EKO compilation/tests and native/main qualification are pending.
