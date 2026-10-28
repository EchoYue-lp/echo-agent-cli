/**
 * Canonical SubagentRun execution-flow store.
 *
 * Consumes the unified `execution://event` channel (kind="subagent"). The
 * legacy `subagent://trace` / `subagent://event` channels and their stores
 * (`subagentTraceStore`, `subagentStore`) were deleted in Phase 4c; this is now
 * the single source of truth for subagent execution-flow events.
 *
 * Aggregation key is the concrete execution id in `subagent_run_id` (normally
 * `{run_id}:{task_id}:{plan_revision}:{attempt}`). `task_id` remains the stable PlanTask join key. This
 * separation keeps retries independent while still allowing task-oriented UI
 * to select the latest attempt.
 */

import { create } from 'zustand';
import type { AgentEventEnvelope } from '../types/api';
import { isCanonicalUsageEvent } from '../components/compress/subagentUsage';
import type {
  ExecEvent as GeneratedExecEvent,
  RuntimeTaskEvent,
  RuntimeEventKind,
  SubagentArtifact,
  SubagentEvidence,
  SubagentStatus,
  SubagentOutcome,
  SubagentTouchedFiles,
  SubagentVerification,
  PlanRevision,
  TaskRun,
  SubagentEventMetadata,
} from '../generated';

export type { SubagentStatus } from '../generated';

/** Event variants carried on execution://event with kind="subagent". */
export type SubagentRunEventKind = Extract<
  RuntimeEventKind,
  | 'started'
  | 'usage'
  | 'isolation_observed'
  | 'thinking_started'
  | 'thinking_delta'
  | 'thinking_ended'
  | 'token_delta'
  | 'note'
  | 'tool_started'
  | 'tool_completed'
  | 'subagent_escalation_requested'
  | 'subagent_stream_gap'
  | 'completed'
  | 'failed'
  | 'timed_out'
  | 'cancelled'
>;

interface WireSubagentArtifact {
  path: string;
  kind: string;
  bytes?: number | string | bigint | null;
  sha256?: string | null;
  producer_execution_id?: string | null;
  available: boolean;
}

type GeneratedExecutionAddress = Pick<
  GeneratedExecEvent,
  'workspace_id' | 'conversation_id' | 'run_id'
>;

/** UI projection derived from the generated Rust `ExecEvent` contract. */
export type ExecutionEvent = GeneratedExecutionAddress & {
  kind: 'subagent';
  subagent_run_id: string;
  agent: string;
  event: SubagentRunEventKind;
  task_id?: string;
  plan_revision?: number;
  attempt?: number;
  // event-specific fields (any of the below may be absent depending on `event`)
  parent?: string;
  task?: string;
  mode?: string;
  prompt_tokens?: number;
  completion_tokens?: number;
  duration_ms?: number;
  tokens_used?: number;
  iteration_count?: number;
  output?: string;
  error?: string;
  prompt_source?: string;
  isolation_requested?: string;
  isolation_observed?: string;
  context_in?: string;
  returns?: string;
  // LLM cache diagnostics (present on `usage` events, emitted per model call)
  message_id?: string;
  model?: string;
  total_tokens?: number;
  cached_prompt_tokens?: number;
  cache_creation_prompt_tokens?: number;
  usage_reported?: boolean;
  usage_event_id?: string;
  /** Background dispatch flag (from DispatchStarted). */
  background?: boolean;
  /** Millisecond timestamp supplied by durable TaskRuntime event replay. */
  started_at?: number;
  /** Parent-facing summary on completed events. */
  summary?: string;
  contract_version?: number;
  terminal_status?: 'completed' | 'failed' | 'cancelled' | 'timed_out';
  artifacts?: WireSubagentArtifact[];
  evidence?: SubagentEvidence[];
  verification?: SubagentVerification[];
  remaining_work?: string[];
  touched_files?: SubagentTouchedFiles;
  /** Original canonical envelope, retained without flattening phase or provenance. */
  agent_envelope?: AgentEventEnvelope;
  native_decode_error?: string;
  runtime_sequence?: string;
  journal_stream_id?: string;
  journal_sequence?: number;
  runtime_replay?: boolean;
  live_observed?: boolean;
  framework_event?: SubagentEventMetadata | null;
  event_id?: string;
  stream_id?: string;
  sequence?: number;
  timestamp?: string;
  parent_event_id?: string;
  requested_after?: number;
  available_from?: number | null;
  latest_sequence?: number;
  [key: string]: unknown;
};

export interface SubagentRunState {
  subagentRunId: string;
  runId: string;
  /** Workspace captured from the lifecycle event; controls must target this
   * run's immutable workspace instead of the currently focused TaskRun. */
  workspaceId?: string;
  taskId?: string;
  planRevision?: number;
  attempt?: number;
  agent: string;
  parent?: string;
  task?: string;
  mode?: string;
  /** Conversation this run belongs to (captured from run_started's
   * conversation_id). Used by TaskRuntimePanel to show ALL inline subagent runs
   * in the current conversation, not just the single activeRun (P1.0: each
   * inline subagent now has its own run_id). */
  conversationId?: string;
  status: SubagentStatus;
  startedAt: number;
  durationMs?: number;
  tokensUsed?: number;
  iterationCount?: number;
  /** Full terminal model output. The presentation layer removes the protocol envelope. */
  finalOutput?: string;
  error?: string;
  promptSource?: string;
  isolationRequested?: string;
  isolationObserved?: string;
  contextIn?: string;
  returns?: string;
  /** Message id that triggered the run (chat message_key); pins this run to a
   * chat message block. Absent for non-chat paths (cron). */
  messageId?: string;
  /** True when started via background dispatch (agent_tool background=true or
   * role is_background). Completion raises a toast without duplicating chat. */
  background?: boolean;
  /** Runtime-owned terminal outcome. Absent while running. */
  outcome?: SubagentOutcome;
  /** Accumulated LLM usage across all model calls in this run (for cache diagnostics). */
  usageEvents?: ExecutionEvent[];
  /** Bounded lifecycle and usage event log. Tool details live in toolExecutionStore. */
  events: ExecutionEvent[];
}

interface SubagentRunStore {
  runs: Record<string, SubagentRunState>;
  /** Ingest one `execution://event` payload (kind must be "subagent"). */
  ingest: (ev: ExecutionEvent) => void;
  clear: () => void;
}

const MAX_EVENTS_PER_RUN = 300;
const STORED_SUBAGENT_EVENTS = new Set<string>([
  'started',
  'usage',
  'isolation_observed',
  'thinking_started',
  'thinking_delta',
  'thinking_ended',
  'token_delta',
  'note',
  'tool_started',
  'tool_completed',
  'subagent_escalation_requested',
  'subagent_stream_gap',
  'completed',
  'failed',
  'timed_out',
  'cancelled',
]);

export function subagentRunStoreKey(runId: string, subagentRunId: string): string {
  return `${runId}\u0000${subagentRunId}`;
}

function statusFromEvent(event: SubagentRunEventKind): SubagentStatus | null {
  switch (event) {
    case 'started':
      return 'running';
    case 'completed':
      return 'completed';
    case 'failed':
      return 'failed';
    case 'timed_out':
      return 'timed_out';
    case 'cancelled':
      return 'cancelled';
    default:
      return null;
  }
}

function artifactBytes(value: WireSubagentArtifact['bytes']): bigint | null {
  if (typeof value === 'bigint') return value;
  if (typeof value === 'number' && Number.isFinite(value) && value >= 0) {
    return BigInt(Math.trunc(value));
  }
  if (typeof value === 'string') {
    try {
      const parsed = BigInt(value);
      return parsed >= 0n ? parsed : null;
    } catch {
      return null;
    }
  }
  return null;
}

function normalizeArtifact(artifact: WireSubagentArtifact): SubagentArtifact {
  return {
    path: artifact.path,
    kind: artifact.kind,
    bytes: artifactBytes(artifact.bytes),
    sha256: artifact.sha256 ?? null,
    producer_execution_id: artifact.producer_execution_id ?? null,
    available: artifact.available,
  };
}

function terminalOutcome(
  ev: ExecutionEvent,
  status: SubagentStatus | null
): SubagentOutcome | undefined {
  if (!status || status === 'running') return undefined;
  const summary =
    (typeof ev.summary === 'string' && ev.summary.trim()) ||
    (typeof ev.error === 'string' && ev.error.trim()) ||
    '';
  return {
    contract_version: typeof ev.contract_version === 'number' ? ev.contract_version : 0,
    status,
    summary,
    artifacts: Array.isArray(ev.artifacts) ? ev.artifacts.map(normalizeArtifact) : [],
    evidence: Array.isArray(ev.evidence) ? ev.evidence : [],
    verification: Array.isArray(ev.verification) ? ev.verification : [],
    remaining_work: Array.isArray(ev.remaining_work) ? ev.remaining_work : [],
    touched_files:
      ev.touched_files && typeof ev.touched_files === 'object'
        ? ev.touched_files
        : { read: [], written: [] },
  };
}

type JsonRecord = Record<string, unknown>;

function jsonRecord(value: unknown): JsonRecord | null {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
    ? (value as JsonRecord)
    : null;
}

function jsonString(value: unknown): string | undefined {
  return typeof value === 'string' ? value : undefined;
}

function jsonNumber(value: unknown): number | undefined {
  return typeof value === 'number' && Number.isFinite(value) ? value : undefined;
}

function jsonStringArray(value: unknown): string[] {
  return Array.isArray(value)
    ? value.filter((item): item is string => typeof item === 'string')
    : [];
}

type TerminalSubagentRunEventKind = 'completed' | 'failed' | 'timed_out' | 'cancelled';

function runtimeTerminalEvent(value: unknown): TerminalSubagentRunEventKind | null {
  switch (value) {
    case 'completed':
    case 'failed':
    case 'timed_out':
    case 'cancelled':
      return value;
    default:
      return null;
  }
}

function runtimeArtifacts(value: unknown): WireSubagentArtifact[] {
  if (!Array.isArray(value)) return [];
  const artifacts: WireSubagentArtifact[] = [];
  for (const item of value) {
    const artifact = jsonRecord(item);
    const path = jsonString(artifact?.path);
    const kind = jsonString(artifact?.kind);
    if (!artifact || !path || !kind) continue;
    artifacts.push({
      path,
      kind,
      bytes:
        typeof artifact.bytes === 'number' ||
        typeof artifact.bytes === 'string' ||
        typeof artifact.bytes === 'bigint'
          ? artifact.bytes
          : null,
      sha256: jsonString(artifact.sha256) ?? null,
      producer_execution_id: jsonString(artifact.producer_execution_id) ?? null,
      available: typeof artifact.available === 'boolean' ? artifact.available : false,
    });
  }
  return artifacts;
}

function runtimeVerification(value: unknown): SubagentVerification[] {
  if (!Array.isArray(value)) return [];
  const verification: SubagentVerification[] = [];
  for (const item of value) {
    const candidate = jsonRecord(item);
    const check = jsonString(candidate?.check);
    const details = jsonString(candidate?.details);
    const status = candidate?.status;
    const source = candidate?.source;
    if (
      !check ||
      details === undefined ||
      (status !== 'passed' && status !== 'failed' && status !== 'not_run') ||
      (source !== 'observed' && source !== 'reported')
    ) {
      continue;
    }
    verification.push({ check, details, status, source });
  }
  return verification;
}

function runtimeTouchedFiles(value: unknown): SubagentTouchedFiles | undefined {
  const touchedFiles = jsonRecord(value);
  if (!touchedFiles) return undefined;
  return {
    read: jsonStringArray(touchedFiles.read),
    written: jsonStringArray(touchedFiles.written),
  };
}

/** Convert the generated Rust wire contract into the view-only event shape. */
export function subagentExecutionEventFromWire(
  event: GeneratedExecEvent,
  journal?: { event_id: string; stream_id: string; sequence: number; timestamp: string }
): ExecutionEvent | null {
  if (
    event.scope !== 'subagent' ||
    !event.subagent_run_id ||
    !STORED_SUBAGENT_EVENTS.has(event.event)
  ) {
    return null;
  }
  const payload = jsonRecord(event.payload) ?? {};
  if (event.event === 'note' && payload.kind !== 'native_agent_event') return null;
  const original = jsonRecord(payload.agent_envelope);
  if (
    event.event === 'note' &&
    original &&
    (original.run_id !== event.run_id ||
      original.conversation_id !== event.conversation_id ||
      original.execution_id !== event.subagent_run_id ||
      original.turn_id !== event.subagent_run_id)
  )
    return null;
  const native = nativeSubagentEnvelope(payload.agent_envelope);
  const decodeError =
    event.event === 'note' && !native ? '原生事件不可读，原始记录仍保留' : undefined;
  const outcome = jsonRecord(payload.outcome);
  const metadata = event.framework_event;
  const terminalStatus = runtimeTerminalEvent(payload.terminal_status ?? outcome?.status);
  if (
    native &&
    (native.run_id !== event.run_id ||
      native.conversation_id !== event.conversation_id ||
      native.execution_id !== event.subagent_run_id)
  )
    return null;
  const timestamp = native?.timestamp ?? metadata?.timestamp ?? journal?.timestamp;
  const startedAt = timestamp ? runtimeEventTimestamp(timestamp) : undefined;
  const artifacts = runtimeArtifacts(outcome?.artifacts ?? payload.artifacts);
  const verification = runtimeVerification(outcome?.verification ?? payload.verification);
  const touchedFiles = runtimeTouchedFiles(outcome?.touched_files ?? payload.touched_files);
  const evidence = Array.isArray(outcome?.evidence ?? payload.evidence)
    ? ((outcome?.evidence ?? payload.evidence) as SubagentEvidence[])
    : undefined;

  return {
    ...payload,
    kind: 'subagent',
    workspace_id: event.workspace_id,
    conversation_id: event.conversation_id,
    run_id: event.run_id,
    subagent_run_id: event.subagent_run_id,
    task_id: event.task_id ?? metadata?.task_id ?? undefined,
    agent: event.agent ?? metadata?.agent_name ?? 'subagent',
    event: event.event as SubagentRunEventKind,
    parent: metadata?.parent_execution_id ?? undefined,
    plan_revision: metadata?.plan_revision ?? jsonNumber(payload.plan_revision),
    attempt: metadata?.attempt ?? jsonNumber(payload.attempt),
    message_id: metadata?.message_id ?? jsonString(payload.message_id),
    started_at: startedAt,
    task: jsonString(payload.task),
    mode: jsonString(payload.mode),
    duration_ms: jsonNumber(payload.duration_ms),
    tokens_used: jsonNumber(payload.tokens_used),
    iteration_count: jsonNumber(payload.iterations ?? payload.iteration_count),
    output: jsonString(payload.output),
    error: jsonString(payload.error),
    isolation_observed: jsonString(payload.isolation ?? payload.isolation_observed),
    summary: jsonString(outcome?.summary ?? payload.summary),
    contract_version: jsonNumber(outcome?.contract_version ?? payload.contract_version),
    terminal_status: terminalStatus ?? undefined,
    artifacts,
    evidence,
    verification,
    remaining_work: jsonStringArray(outcome?.remaining_work ?? payload.remaining_work),
    touched_files: touchedFiles,
    journal_stream_id: journal?.stream_id,
    journal_sequence: journal?.sequence,
    agent_envelope: native,
    native_decode_error: decodeError,
    framework_event: metadata,
    event_id: native?.event_id ?? metadata?.event_id ?? journal?.event_id,
    stream_id:
      native?.stream_id ??
      metadata?.stream_id ??
      (event.event === 'subagent_stream_gap' ? jsonString(payload.stream_id) : journal?.stream_id),
    // A synthetic gap describes the framework stream but is itself sequenced
    // by ChatEventLog. Do not compare that outer sequence with framework data.
    sequence:
      native?.sequence ??
      metadata?.sequence ??
      (event.event === 'subagent_stream_gap' ? undefined : journal?.sequence),
    timestamp,
    parent_event_id: metadata?.parent_event_id ?? undefined,
    requested_after: jsonNumber(payload.requested_after),
    available_from: payload.available_from === null ? null : jsonNumber(payload.available_from),
    latest_sequence: jsonNumber(payload.latest_sequence),
  };
}

function runtimeEventTimestamp(timestamp: string): number | undefined {
  const value = Date.parse(timestamp);
  return Number.isFinite(value) ? value : undefined;
}

/**
 * Translate durable TaskRuntime boundaries into the existing
 * realtime Subagent lifecycle stream. It restores the same GUI cards after a
 * page reload without introducing a second Subagent state machine.
 */
export function taskRuntimeSubagentExecutionEvents(
  run: TaskRun,
  plan: PlanRevision | null,
  events: readonly RuntimeTaskEvent[]
): ExecutionEvent[] {
  const projected: ExecutionEvent[] = [];
  const agentsByExecution = new Map<string, string>();

  for (const runtimeEvent of events) {
    if (
      runtimeEvent.run_id !== run.run_id ||
      (runtimeEvent.event_type !== 'subagent_assigned' &&
        runtimeEvent.event_type !== 'subagent_released' &&
        runtimeEvent.event_type !== 'note' &&
        runtimeEvent.event_type !== 'tool_started' &&
        runtimeEvent.event_type !== 'tool_completed')
    ) {
      continue;
    }

    const payload = jsonRecord(runtimeEvent.payload);
    if (
      runtimeEvent.event_type === 'tool_started' ||
      runtimeEvent.event_type === 'tool_completed'
    ) {
      const execution = jsonString(payload?.execution_id);
      if (!execution) continue;
      const tool = subagentExecutionEventFromWire({
        workspace_id: run.workspace_id,
        conversation_id: run.conversation_id,
        run_id: run.run_id,
        scope: 'subagent',
        task_id: runtimeEvent.task_id,
        subagent_run_id: execution,
        agent: null,
        event: runtimeEvent.event_type,
        payload,
        framework_event: null,
      });
      if (tool)
        projected.push({
          ...tool,
          event_id: `runtime:${runtimeEvent.seq}`,
          timestamp: runtimeEvent.timestamp,
          runtime_sequence: runtimeEvent.seq,
          runtime_replay: true,
        });
      continue;
    }
    if (runtimeEvent.event_type === 'note') {
      const execution = jsonString(payload?.execution_id) ?? runtimeEvent.step_id;
      if (payload?.kind !== 'native_agent_event' || !execution) continue;
      const native = subagentExecutionEventFromWire({
        workspace_id: run.workspace_id,
        conversation_id: run.conversation_id,
        run_id: run.run_id,
        scope: 'subagent',
        task_id: runtimeEvent.task_id,
        subagent_run_id: execution,
        agent: jsonString(payload.agent) ?? null,
        event: 'note',
        payload,
        framework_event: null,
      });
      if (native)
        projected.push({
          ...native,
          runtime_sequence: runtimeEvent.seq,
          runtime_replay: true,
        });
      continue;
    }
    const executionId = jsonString(payload?.execution_id) ?? runtimeEvent.step_id ?? undefined;
    if (!executionId) continue;

    const taskId = runtimeEvent.task_id ?? undefined;
    const planTask = taskId ? plan?.tasks.find((task) => task.id === taskId) : undefined;
    const task = planTask
      ? planTask.description.trim() || planTask.title.trim() || undefined
      : undefined;
    const startedAt = runtimeEventTimestamp(runtimeEvent.timestamp);

    if (runtimeEvent.event_type === 'subagent_assigned') {
      const agent = jsonString(payload?.agent_name) ?? planTask?.agent_role ?? 'subagent';
      agentsByExecution.set(executionId, agent);
      projected.push({
        kind: 'subagent',
        workspace_id: run.workspace_id,
        subagent_run_id: executionId,
        run_id: run.run_id,
        task_id: taskId,
        agent,
        event: 'started',
        task,
        message_id: run.root_message_id,
        conversation_id: run.conversation_id,
        started_at: startedAt,
        plan_revision: jsonNumber(payload?.plan_revision),
        attempt: jsonNumber(payload?.attempt),
      });
      continue;
    }

    const outcome = jsonRecord(payload?.outcome);
    const terminalEvent = runtimeTerminalEvent(payload?.status ?? outcome?.status);
    if (!terminalEvent) continue;
    const summary = jsonString(outcome?.summary) ?? jsonString(payload?.summary);
    const output = jsonString(payload?.full_output);
    projected.push({
      kind: 'subagent',
      workspace_id: run.workspace_id,
      subagent_run_id: executionId,
      run_id: run.run_id,
      task_id: taskId,
      agent: agentsByExecution.get(executionId) ?? planTask?.agent_role ?? 'subagent',
      event: terminalEvent,
      task,
      message_id: run.root_message_id,
      conversation_id: run.conversation_id,
      started_at: startedAt,
      plan_revision: jsonNumber(payload?.plan_revision),
      attempt: jsonNumber(payload?.attempt),
      output,
      error: terminalEvent === 'failed' ? (summary ?? output) : undefined,
      summary,
      contract_version: jsonNumber(outcome?.contract_version),
      terminal_status: terminalEvent,
      artifacts: runtimeArtifacts(outcome?.artifacts),
      verification: runtimeVerification(outcome?.verification),
      remaining_work: jsonStringArray(outcome?.remaining_work),
      touched_files: runtimeTouchedFiles(outcome?.touched_files),
    });
  }

  return projected;
}

function executionAttempt(run: SubagentRunState): number | null {
  if (run.attempt != null && Number.isSafeInteger(run.attempt)) return run.attempt;
  const segments = run.subagentRunId.split(':');
  const last = segments.at(-1) ?? '';
  const candidate = /^\d+$/.test(last) ? last : (segments.at(-2) ?? '');
  if (!/^\d+$/.test(candidate)) return null;
  const attempt = Number(candidate);
  return Number.isSafeInteger(attempt) ? attempt : null;
}

/** Keep complete history in the store but select one current attempt per task. */
export function latestSubagentRunsByTask(runs: readonly SubagentRunState[]): SubagentRunState[] {
  const latest = new Map<string, SubagentRunState>();
  const ungrouped: SubagentRunState[] = [];

  for (const run of runs) {
    if (!run.taskId) {
      ungrouped.push(run);
      continue;
    }
    const key = `${run.runId}\u0000${run.taskId}`;
    const current = latest.get(key);
    if (!current) {
      latest.set(key, run);
      continue;
    }
    const currentAttempt = executionAttempt(current);
    const nextAttempt = executionAttempt(run);
    const newerAttempt =
      nextAttempt !== null && (currentAttempt === null || nextAttempt > currentAttempt);
    const sameAttemptIsNewer = nextAttempt === currentAttempt && run.startedAt >= current.startedAt;
    if (newerAttempt || sameAttemptIsNewer) latest.set(key, run);
  }

  return [...ungrouped, ...latest.values()];
}

export const useSubagentRunStore = create<SubagentRunStore>((set) => ({
  runs: {},

  ingest: (ev) => {
    set((s) => {
      // TaskRuntime tool/thinking events share kind="subagent" on the wire but
      // belong to their dedicated stores. Keep this lifecycle log type-safe.
      if (!STORED_SUBAGENT_EVENTS.has(ev.event)) return s;
      if (ev.event === 'note' && !ev.agent_envelope && !ev.native_decode_error) return s;
      const id = ev.subagent_run_id;
      const storeKey = subagentRunStoreKey(ev.run_id, id);
      const prev = s.runs[storeKey];
      const newStatus = statusFromEvent(ev.event);
      const duplicate = prev?.events.findIndex(
        (candidate) =>
          (ev.event_id && candidate.event_id === ev.event_id) ||
          ((ev.runtime_replay || candidate.runtime_replay) &&
            ['tool_started', 'tool_completed'].includes(ev.event) &&
            candidate.event === ev.event &&
            candidate.call_id === ev.call_id)
      );
      if (prev && duplicate != null && duplicate >= 0) {
        if (ev.runtime_sequence == null && ev.journal_sequence == null) return s;
        const events = prev.events.map((candidate, index) =>
          index === duplicate
            ? {
                ...ev,
                ...candidate,
                runtime_sequence: ev.runtime_sequence ?? candidate.runtime_sequence,
                runtime_replay: ev.runtime_replay ?? candidate.runtime_replay,
                journal_stream_id: ev.journal_stream_id ?? candidate.journal_stream_id,
                journal_sequence: ev.journal_sequence ?? candidate.journal_sequence,
                live_observed: ev.journal_sequence != null ? false : candidate.live_observed,
              }
            : candidate
        );
        return { runs: { ...s.runs, [storeKey]: { ...prev, events: orderNativeHistory(events) } } };
      }
      // Keep the original stale lifecycle fence while admitting unseen history.
      if (
        prev &&
        newStatus !== null &&
        !ev.runtime_replay &&
        ev.sequence != null &&
        prev.events.some(
          (candidate) =>
            candidate.stream_id === ev.stream_id &&
            candidate.sequence != null &&
            candidate.sequence >= ev.sequence!
        )
      )
        return s;
      // Synthetic TaskRuntime recovery may settle before the canonical chat
      // replay arrives. It cannot be overwritten by another synthetic event,
      // but framework-sequenced history may still fill the timeline and enrich
      // the same terminal outcome.
      const previousTerminal = Boolean(prev && prev.status !== 'running');
      const frameworkSequenced = Boolean(
        ev.framework_event || ev.agent_envelope || ev.runtime_replay || ev.native_decode_error
      );
      if (previousTerminal && !frameworkSequenced) {
        return s;
      }
      const compatibleTerminal =
        !previousTerminal || newStatus === null || newStatus === prev?.status;
      // P1.0: capture conversation_id (present on run_started; carried via the
      // ExecutionEvent's index signature). Persists on the run record so
      // TaskRuntimePanel can group all inline subagent runs per conversation.
      const evConvId = ev.conversation_id as string | undefined;
      // Lazily create the run on first sight (any event may arrive first in
      // principle, though `started` normally does).
      const run: SubagentRunState = prev ?? {
        subagentRunId: id,
        runId: ev.run_id,
        workspaceId: ev.workspace_id,
        taskId: ev.task_id,
        planRevision: ev.plan_revision,
        attempt: ev.attempt,
        agent: ev.agent,
        parent: ev.parent,
        task: ev.task,
        mode: ev.mode,
        conversationId: evConvId,
        status: 'running',
        startedAt:
          typeof ev.started_at === 'number' && Number.isFinite(ev.started_at)
            ? ev.started_at
            : Date.now(),
        events: [],
        usageEvents: [],
      };
      // Append the event, capping the log.
      const olderSource =
        ev.sequence != null &&
        run.events.some(
          (candidate) =>
            candidate.stream_id === ev.stream_id &&
            candidate.sequence != null &&
            candidate.sequence > ev.sequence!
        );
      const incoming = {
        ...ev,
        live_observed:
          ev.live_observed ??
          (!ev.runtime_replay &&
            ev.journal_sequence == null &&
            !olderSource &&
            Boolean(
              ev.agent_envelope ||
              ev.framework_event ||
              ['tool_started', 'tool_completed'].includes(ev.event)
            )),
      };
      const events = orderNativeHistory([...run.events, incoming]);
      if (events.length > MAX_EVENTS_PER_RUN) events.splice(0, events.length - MAX_EVENTS_PER_RUN);
      // Accumulate LLM usage events separately (uncapped, but bounded by the
      // number of model calls — typically small) for cache-diagnostics panels.
      const usageEvents = isCanonicalUsageEvent(ev)
        ? [...(run.usageEvents ?? []), ev]
        : run.usageEvents;
      const outcome =
        (compatibleTerminal ? terminalOutcome(ev, newStatus) : undefined) ?? run.outcome;
      const next: SubagentRunState = {
        ...run,
        workspaceId: ev.workspace_id || run.workspaceId,
        // Preserve any field present on the event (overwrites prev).
        taskId: ev.task_id ?? run.taskId,
        planRevision: ev.plan_revision ?? run.planRevision,
        attempt: ev.attempt ?? run.attempt,
        parent: ev.parent ?? run.parent,
        task: ev.task ?? run.task,
        mode: ev.mode ?? run.mode,
        conversationId: evConvId ?? run.conversationId,
        status: compatibleTerminal ? (newStatus ?? run.status) : run.status,
        durationMs: ev.duration_ms ?? run.durationMs,
        tokensUsed: ev.tokens_used ?? run.tokensUsed,
        iterationCount: ev.iteration_count ?? run.iterationCount,
        finalOutput: compatibleTerminal ? (ev.output ?? run.finalOutput) : run.finalOutput,
        error: compatibleTerminal ? (ev.error ?? run.error) : run.error,
        promptSource: ev.prompt_source ?? run.promptSource,
        isolationRequested: ev.isolation_requested ?? run.isolationRequested,
        isolationObserved: ev.isolation_observed ?? run.isolationObserved,
        contextIn: ev.context_in ?? run.contextIn,
        returns: ev.returns ?? run.returns,
        messageId: ev.message_id ?? run.messageId,
        background: typeof ev.background === 'boolean' ? ev.background : run.background,
        outcome,
        usageEvents,
        events,
      };
      return { runs: { ...s.runs, [storeKey]: next } };
    });
  },

  clear: () => set({ runs: {} }),
}));

/** Shared live/replay ingress for the generated Rust execution contract. */
export function ingestSubagentExecEvent(
  event: GeneratedExecEvent,
  journal?: { event_id: string; stream_id: string; sequence: number; timestamp: string }
): void {
  const projected = subagentExecutionEventFromWire(event, journal);
  if (projected) useSubagentRunStore.getState().ingest(projected);
}

export function ingestTaskRuntimeSubagentEvents(
  run: TaskRun,
  plan: PlanRevision | null,
  events: readonly RuntimeTaskEvent[]
): void {
  for (const event of taskRuntimeSubagentExecutionEvents(run, plan, events)) {
    useSubagentRunStore.getState().ingest(event);
  }
}

/** Native observations never provide lifecycle status to statusFromEvent. */
export function nativeSubagentEnvelope(value: unknown): AgentEventEnvelope | undefined {
  const envelope = jsonRecord(value);
  const payload = jsonRecord(envelope?.payload);
  if (
    !envelope ||
    !payload ||
    typeof envelope.schema_version !== 'number' ||
    !Number.isSafeInteger(envelope.schema_version) ||
    typeof envelope.content_hash !== 'string' ||
    typeof envelope.timestamp !== 'string' ||
    typeof envelope.event_id !== 'string' ||
    typeof envelope.stream_id !== 'string' ||
    typeof envelope.turn_id !== 'string' ||
    typeof envelope.run_id !== 'string' ||
    typeof envelope.conversation_id !== 'string' ||
    typeof envelope.execution_id !== 'string' ||
    typeof envelope.sequence !== 'number' ||
    !Number.isSafeInteger(envelope.sequence)
  )
    return undefined;
  const data = jsonRecord(payload.data);
  if (payload.type === 'assistant_message') {
    const block = jsonRecord(data?.block);
    if (
      !block ||
      typeof block.block_id !== 'string' ||
      typeof block.request_id !== 'string' ||
      typeof block.delta !== 'string' ||
      typeof block.complete !== 'boolean' ||
      !['reasoning', 'commentary', 'partial_answer', 'final_answer', 'unknown'].includes(
        String(block.phase)
      )
    )
      return undefined;
  } else if (payload.type === 'execution_status') {
    if (
      !data ||
      typeof data.execution_id !== 'string' ||
      ![
        'started',
        'waiting',
        'tool_pending',
        'candidate',
        'settled',
        'cancelled',
        'failed',
      ].includes(String(data.status)) ||
      (data.reason != null && typeof data.reason !== 'string')
    )
      return undefined;
  } else if (payload.type === 'error') {
    if (!data || typeof data.message !== 'string') return undefined;
  } else if (payload.type === 'final_answer') {
    if (typeof payload.data !== 'string') return undefined;
  } else if (payload.type !== 'cancelled') return undefined;
  return value as AgentEventEnvelope;
}

/** Stable presentation merge of original sequence facts, never execution scheduling. */
function orderNativeHistory(events: ExecutionEvent[]): ExecutionEvent[] {
  const links = events.map(() => new Set<number>());
  const incoming = events.map(() => 0);
  const connect = (from: number, to: number) => {
    const edges = links[from];
    if (from === to || edges.has(to)) return;
    edges.add(to);
    incoming[to] += 1;
  };
  const join = (indices: number[]) =>
    indices.forEach((id, index) => {
      if (index > 0) connect(indices[index - 1], id);
    });
  join(
    events.map((event, index) => (event.live_observed ? index : -1)).filter((index) => index >= 0)
  );
  join(
    events
      .map((event, index) => (event.runtime_sequence != null ? index : -1))
      .filter((index) => index >= 0)
      .sort((a, b) =>
        compareRuntimeSequence(events[a].runtime_sequence!, events[b].runtime_sequence!)
      )
  );
  const journalStreams = new Map<string, number[]>();
  events.forEach((event, index) => {
    if (event.journal_stream_id && event.journal_sequence != null) {
      journalStreams.set(event.journal_stream_id, [
        ...(journalStreams.get(event.journal_stream_id) ?? []),
        index,
      ]);
    }
  });
  journalStreams.forEach((indices) =>
    join(indices.sort((a, b) => events[a].journal_sequence! - events[b].journal_sequence!))
  );
  const streams = new Map<string, number[]>();
  events.forEach((event, index) => {
    const envelope = event.agent_envelope ?? event.framework_event;
    if (!envelope) return;
    const key = JSON.stringify([envelope.stream_id, envelope.turn_id, envelope.execution_id]);
    streams.set(key, [...(streams.get(key) ?? []), index]);
  });
  streams.forEach((indices) =>
    join(
      indices.sort(
        (a, b) =>
          (events[a].agent_envelope ?? events[a].framework_event)!.sequence -
          (events[b].agent_envelope ?? events[b].framework_event)!.sequence
      )
    )
  );
  const ready = incoming.flatMap((count, index) => (count === 0 ? [index] : []));
  const ordered: ExecutionEvent[] = [];
  while (ready.length) {
    const index = ready.shift()!;
    ordered.push(events[index]);
    links[index].forEach((next) => {
      incoming[next] -= 1;
      if (incoming[next] !== 0) return;
      const place = ready.findIndex((value) => value > next);
      if (place < 0) ready.push(next);
      else ready.splice(place, 0, next);
    });
  }
  return ordered.length === events.length ? ordered : events;
}

function compareRuntimeSequence(left: string, right: string): number {
  if (!/^\d+$/.test(left) || !/^\d+$/.test(right)) return 0;
  const a = BigInt(left),
    b = BigInt(right);
  return a < b ? -1 : a > b ? 1 : 0;
}
