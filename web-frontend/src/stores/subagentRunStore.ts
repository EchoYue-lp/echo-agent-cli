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
import { isCanonicalUsageEvent } from '../components/compress/subagentUsage';
import type {
  RuntimeTaskEvent,
  SubagentArtifactResult,
  SubagentRunStatus,
  SubagentTaskResult,
  SubagentTouchedFiles,
  SubagentVerificationResult,
  TaskPlan,
  TaskRun,
} from '../generated';

export type { SubagentRunStatus } from '../generated';

/** Event variants carried on execution://event with kind="subagent". */
export type SubagentRunEventKind =
  | 'started'
  | 'usage' // canonical DispatchLlmUsage event with provider/cache metadata
  | 'isolation_observed'
  | 'completed'
  | 'failed'
  | 'timed_out'
  | 'cancelled';

interface WireSubagentArtifactResult {
  path: string;
  kind: string;
  bytes?: number | string | bigint | null;
  sha256?: string | null;
  producer_execution_id?: string | null;
  available: boolean;
}

/** One raw event on the wire (the bridge emits these as a serde_json::Object). */
export interface ExecutionEvent {
  kind: 'subagent';
  workspace_id: string;
  conversation_id: string;
  subagent_run_id: string;
  run_id: string;
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
  artifacts?: WireSubagentArtifactResult[];
  verification?: SubagentVerificationResult[];
  remaining_work?: string[];
  touched_files?: SubagentTouchedFiles;
  [key: string]: unknown;
}

export interface SubagentRunState {
  subagentRunId: string;
  runId: string;
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
  status: SubagentRunStatus;
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
  /** Runtime-owned terminal result. Absent while running. */
  result?: SubagentTaskResult;
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
  'completed',
  'failed',
  'timed_out',
  'cancelled',
]);

export function subagentRunStoreKey(runId: string, subagentRunId: string): string {
  return `${runId}\u0000${subagentRunId}`;
}

function statusFromEvent(event: SubagentRunEventKind): SubagentRunStatus | null {
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

function artifactBytes(value: WireSubagentArtifactResult['bytes']): bigint | null {
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

function normalizeArtifact(artifact: WireSubagentArtifactResult): SubagentArtifactResult {
  return {
    path: artifact.path,
    kind: artifact.kind,
    bytes: artifactBytes(artifact.bytes),
    sha256: artifact.sha256 ?? null,
    producer_execution_id: artifact.producer_execution_id ?? null,
    available: artifact.available,
  };
}

function terminalResult(
  ev: ExecutionEvent,
  status: SubagentRunStatus | null
): SubagentTaskResult | undefined {
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

function runtimeArtifacts(value: unknown): WireSubagentArtifactResult[] {
  if (!Array.isArray(value)) return [];
  const artifacts: WireSubagentArtifactResult[] = [];
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

function runtimeVerification(value: unknown): SubagentVerificationResult[] {
  if (!Array.isArray(value)) return [];
  const verification: SubagentVerificationResult[] = [];
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

function runtimeEventTimestamp(timestamp: string): number | undefined {
  const value = Date.parse(timestamp);
  return Number.isFinite(value) ? value : undefined;
}

/**
 * Thin application adapter from durable TaskRuntime boundaries to the existing
 * realtime Subagent lifecycle stream. It restores the same GUI cards after a
 * page reload without introducing a second Subagent state machine.
 */
export function taskRuntimeSubagentExecutionEvents(
  run: TaskRun,
  plan: TaskPlan | null,
  events: readonly RuntimeTaskEvent[]
): ExecutionEvent[] {
  const projected: ExecutionEvent[] = [];
  const agentsByExecution = new Map<string, string>();

  for (const runtimeEvent of events) {
    if (
      runtimeEvent.run_id !== run.run_id ||
      (runtimeEvent.event_type !== 'subagent_assigned' &&
        runtimeEvent.event_type !== 'subagent_released')
    ) {
      continue;
    }

    const payload = jsonRecord(runtimeEvent.payload);
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

    const result = jsonRecord(payload?.result);
    const terminalEvent = runtimeTerminalEvent(payload?.status ?? result?.status);
    if (!terminalEvent) continue;
    const summary = jsonString(result?.summary) ?? jsonString(payload?.summary);
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
      contract_version: jsonNumber(result?.contract_version),
      terminal_status: terminalEvent,
      artifacts: runtimeArtifacts(result?.artifacts),
      verification: runtimeVerification(result?.verification),
      remaining_work: jsonStringArray(result?.remaining_work),
      touched_files: runtimeTouchedFiles(result?.touched_files),
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
      const id = ev.subagent_run_id;
      const storeKey = subagentRunStoreKey(ev.run_id, id);
      const prev = s.runs[storeKey];
      const newStatus = statusFromEvent(ev.event);
      // One execution id has one monotonic lifecycle. Retries use a new
      // `{run_id}:{task_id}:{plan_revision}:{attempt}:{claim_id}` id, so late/duplicate events
      // must not reopen a terminal execution or overwrite its result.
      if (prev && prev.status !== 'running') {
        return s;
      }
      // P1.0: capture conversation_id (present on run_started; carried via the
      // ExecutionEvent's index signature). Persists on the run record so
      // TaskRuntimePanel can group all inline subagent runs per conversation.
      const evConvId = ev.conversation_id as string | undefined;
      // Lazily create the run on first sight (any event may arrive first in
      // principle, though `started` normally does).
      const run: SubagentRunState = prev ?? {
        subagentRunId: id,
        runId: ev.run_id,
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
      const events =
        run.events.length >= MAX_EVENTS_PER_RUN
          ? [...run.events.slice(run.events.length - MAX_EVENTS_PER_RUN + 1), ev]
          : [...run.events, ev];
      // Accumulate LLM usage events separately (uncapped, but bounded by the
      // number of model calls — typically small) for cache-diagnostics panels.
      const usageEvents = isCanonicalUsageEvent(ev)
        ? [...(run.usageEvents ?? []), ev]
        : run.usageEvents;
      const result = terminalResult(ev, newStatus) ?? run.result;
      const next: SubagentRunState = {
        ...run,
        // Preserve any field present on the event (overwrites prev).
        taskId: ev.task_id ?? run.taskId,
        planRevision: ev.plan_revision ?? run.planRevision,
        attempt: ev.attempt ?? run.attempt,
        parent: ev.parent ?? run.parent,
        task: ev.task ?? run.task,
        mode: ev.mode ?? run.mode,
        conversationId: evConvId ?? run.conversationId,
        status: newStatus ?? run.status,
        durationMs: ev.duration_ms ?? run.durationMs,
        tokensUsed: ev.tokens_used ?? run.tokensUsed,
        iterationCount: ev.iteration_count ?? run.iterationCount,
        finalOutput: ev.output ?? run.finalOutput,
        error: ev.error ?? run.error,
        promptSource: ev.prompt_source ?? run.promptSource,
        isolationRequested: ev.isolation_requested ?? run.isolationRequested,
        isolationObserved: ev.isolation_observed ?? run.isolationObserved,
        contextIn: ev.context_in ?? run.contextIn,
        returns: ev.returns ?? run.returns,
        messageId: ev.message_id ?? run.messageId,
        background: typeof ev.background === 'boolean' ? ev.background : run.background,
        result,
        usageEvents,
        events,
      };
      return { runs: { ...s.runs, [storeKey]: next } };
    });
  },

  clear: () => set({ runs: {} }),
}));

export function ingestTaskRuntimeSubagentEvents(
  run: TaskRun,
  plan: TaskPlan | null,
  events: readonly RuntimeTaskEvent[]
): void {
  for (const event of taskRuntimeSubagentExecutionEvents(run, plan, events)) {
    useSubagentRunStore.getState().ingest(event);
  }
}
