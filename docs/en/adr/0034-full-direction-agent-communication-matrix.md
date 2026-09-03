# ADR 0034: Full-Direction Agent Communication Matrix and Conversation Collaboration Tools

- Status: Accepted
- Date: 2026-09-02
- Owners: `agent_control`, `tasks/task_runtime`, `state/app_state`

## Context

ADR 0001 section 4.9 documented the full Codex communication matrix, but the
ADR 0016 implementation adopted only the control-plane half. Conversation to
conversation (`agent_message`) and host to running Subagent (`agent_message`
with a TaskSubagent target plus steer) worked. Running Subagent to parent,
Subagent to sibling, and Subagent to its own child were missing because every
built-in role defaulted `can_delegate` to false. The four conversation-plane
tools designed in section 7.4, `agent_spawn`, `agent_resume`, `agent_handoff`,
and `agent_group`, were also absent.

The development-stage product decision is to implement every direction and all
four tools. `agent_handoff` means an in-process cross-workspace migration, all
eight built-in roles enable `can_delegate`, and `agent_spawn` may target any
registered workspace.

## Decision

### Layering gate

**Framework (`echo-agent`, ADR 0027):** `SubagentLineage` propagates lineage
identity through the context chain; `SubagentUplinkFn` is the uplink primitive
whose default sink publishes to the event bus and shared control plane;
`SubagentControlRegistry` mounts a shared `SubagentRegistry`; built-in
`subagent_message` and `subagent_list` tools address siblings through
`SubagentPeerAddress::{ByExecutionId, ByTaskId}`.

**Application (this ADR):** EKO owns uplink routing policy (journal, pause, and
sibling delivery), the four conversation tools, prompt protocol, role
frontmatter, and the tool exclusion list.

### Subagent plane

1. **EKO uplink sink** (`tasks/task_runtime/uplink.rs`): `execute_task` creates
   `eko_uplink_sink(store)` and injects it through `ExternalRunContext.uplink`
   into both readonly and writer dispatch paths. `SubagentLineage` stamps role,
   run, and plan revision; framework admission fills task, attempt, and
   execution identity.
   - Parent `report` journals `SubagentEscalationRequested{blocking:false}` and
     does not affect scheduling.
   - Parent `escalate` journals `blocking:true` and calls
     `request_pause_with_reason(NeedsInput)`. The sender continues best-effort
     work rather than waiting on its parent. User input returns to the exact
     attempt through the existing durable guidance/live-steer path.
   - Sibling `ByExecutionId` performs live delivery through
     `SubagentControlService.send_message`.
   - Sibling `ByTaskId` computes the next attempt as
     `max(latest, retry) + 1`, then queues durable `NextAttempt` guidance for
     delivery during dispatch admission.
2. **Tool composition:** readonly and writer builders append
   `register_subagent_message_tools()`. All eight built-in role files declare
   `can_delegate: true`; `NestedDelegationPolicy` still bounds nesting to three
   levels by default.
3. **Prompt protocol** (`subagent_prompt.rs`, Subagent Protocol): Subagents do
   not talk directly to users or request user approval. When blocked, they use
   `subagent_message` escalation and continue working. Sibling messages are
   unverified claims, not evidence.

### Conversation plane

All tools mount on the shared ToolManager, so GUI, TUI, CLI, and channels gain
the same capabilities.

4. **`agent_spawn(goal, title?, workspace_id?, first_message?, start?)`:**
   `AgentControlAppOps`, implemented by `AppState` and injected once, creates a
   conversation in the current or any registered workspace. With `start=true`,
   the first message cold-starts through enqueue plus delivery-supervisor wake.
5. **`agent_resume(workspace_id, conversation_id, resume_policy, run_id?, text?)`:**
   `followup` queues a follow-up and wakes delivery. `task_run` captures an
   exact `TaskRunResumeIdentity` from current run state and launches planned
   resume through the pooled conversation Agent. Task-run resume is limited to
   the current workspace; cross-workspace work must migrate first.
6. **`agent_handoff(workspace_id, conversation_id, destination_workspace_id, follow_up?)`:**
   In-process cross-workspace migration recreates the same conversation ID and
   transcript in the destination store. When the source is current, EKO first
   retires the pooled conversation Agent, then deletes the source conversation.
   An optional follow-up is delivered at the destination. Inbox addressing is
   `(workspace, conversation)` and old inbox state converges through retention.
7. **`agent_group(action: list|create|update|delete, ...)`:** directly uses the
   existing `AgentRouter` group authority (`groups.json`, validation, and CRUD)
   with no second source of truth. `agent_list` adds `group_id` filtering for
   leaders and members.
8. **Exclusion list:** `TASK_CONTROL_TOOLS` expands to eight entries.
   `agent_spawn`, `agent_group`, `agent_resume`, and `agent_handoff` cannot be
   delegated through PlanTask `allowed_tools`. Like the existing six-tool
   suite, the four tools remain deferred behind `tool_search`, preserving the
   first-turn schema budget.

## Trade-offs

- Blocking escalation pauses run-level scheduling, not the sending attempt.
  This matches the queue-only behavior of Codex `trigger_turn=false` and avoids
  parent-child mutual waits.
- Framework's default sink handles ordinary nested trees by steering the parent
  and publishing events. TaskRun contexts inject the EKO sink for journal and
  pause policy; application injection takes precedence.
- Handoff does not migrate TaskRun ownership because `runtime_for_target`
  remains workspace-bound. Cross-workspace continuation should use a frozen
  `TaskExecutionTarget`, or migrate the conversation before creating a new run.

## Consequences

- Five directions work: conversation to conversation, host to Subagent,
  Subagent to parent, Subagent to sibling, and Subagent to child.
- New journal event `SubagentEscalationRequested` is attention-level and
  surfaces as a lifecycle notification. `SubagentControlActorSource::Peer`
  records Subagent-originated commands.
- All nine tools designed in ADR 0001 section 7.4 now exist. The remaining
  deferred increments are worktree parameters for `agent_spawn` and
  cross-host semantics for `agent_handoff`.

## Verification

- Unit coverage includes the four uplink routes (report, pausing escalation,
  queued ByTaskId, and rejection without run context), rejection of
  `agent_spawn` by `TASK_CONTROL_TOOLS`, eight delegating roles, and prompt
  protocol uniqueness.
- `tests/f10_agent_communication_matrix.rs` covers production wiring, group
  CRUD, and failure when application operations are unavailable.
- The complete `echo-agent-cli` pre-commit gates from `AGENTS.md` apply.
