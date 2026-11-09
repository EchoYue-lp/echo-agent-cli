# ADR 0052: GUI-configurable EKO Subagent depth

## Status

Accepted product policy; source implementation awaits Rust/native qualification.
- Date: 2026-10-24
- Owners: EKO configuration, TaskRuntime, shared chat execution

## Context

ADR0034 enabled nested delegation and described a default of three; ADR0039
required one fixed child layer. The user resolved that conflict explicitly:
EKO's GUI selects 1, 2 or 3 child layers, defaults to 1, and never admits layer 4.
Primary is layer 0; the first actual child is layer 1. This is separate from
concurrency, model turns and graph dependencies. The framework remains reusable
with independently configured nesting; SDK accepted contracts do not change.

## Decision

EKO owns the validated `max_subagent_depth` setting, outside Framework
AgentSettings. GUI, IPC and YAML accept only integers 1..3. The original owned
configuration mutation persists the setting atomically; save failures remain
errors. The settings panel says that the value affects new tasks.

A shared in-memory projection of committed configuration supplies new-run/turn
admissions only. It is not another persistent authority. A new TaskRun records
its immutable `TaskRunExecutionProfile.subagent_depth_limit` in the original
RunCreated journal; the existing fold, checkpoint, snapshot and resume identity
retain it. Same-run new PlanTasks, fixes, retries and recovery use that binding.
A GUI change from 1 to 3 or 3 to 1 never edits existing run profiles or cancels
accepted physical execution. Explicit conflicting create-or-read identities fail.

A lightweight turn captures the limit in its original invocation context without
creating a TaskRun. Primary policy is depth 0; RuntimeTaskService supplies the
actual target child at depth 1. Exact-attempt binding and Fork carry that value,
without advancing twice. Roles can narrow permission, but static role flags do
not override the per-execution limit. Missing historical bindings remain missing:
observation, cancellation and original settlement remain possible, while new
delegation/resume is refused through existing recovery/error boundaries rather
than borrowing the current GUI value or Framework's fallback.

The original nonempty PlanTask allowed_tools bound also applies to primary
Verification and every descendant. Verification remains the existing primary
route, not a synthetic Subagent called primary. The original invocation carries
its positive bound to the framework snapshot taken after the execution lock;
complements of previously registered names are insufficient. Search activation,
fresh roles and queued dynamic registration cannot widen that bound. Framework
inheritance repairs remain with their framework owner; no executor, validator,
store, terminal state or user-operated collaboration console is introduced.

## Alternatives

A permanently disabled child delegation flag would break limits 2/3 and active
runs bound to 3. A global framework default would affect unrelated consumers.
Re-reading GUI settings on each tool call would mutate running task authority.
All three alternatives are rejected. AI-controlled messages, parallel work and
one revisioned task graph remain; reaching the limit can return a scoped request
for the original parent to organize further work.

## Consequences

This ADR supersedes only ADR0034's unconditional nesting/default-three policy
and related test expectations, and ADR0039's unconditional fixed-single-layer
policy. Their communication, lifecycle and GUI ownership decisions remain.
ADR0038 unified-task remains Proposed; this record does not accept it wholesale.

## Validation and publication boundary

Source tests define config range/persistence, the 1/2/3 depth boundaries,
create-or-read identity and journal/checkpoint-tail recovery. Frontend focused
checks do not prove Rust typing, native GUI behavior, real effects or main CI.
Rust exports must be generated from the authorized exact source later; generated
contracts are not edited by hand. Website mirroring belongs to its sole root
writer and must wait for the actually accepted source. Existing framework/SDK
examples need no EKO-setting API; the EKO YAML example is updated in place.
