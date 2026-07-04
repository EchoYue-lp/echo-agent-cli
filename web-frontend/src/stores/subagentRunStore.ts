/**
 * SubagentRun store — Phase 4c of the Subagent unification
 * (spec: docs/subagent-unification-plan.md §6).
 *
 * Consumes the unified `execution://event` channel (kind="subagent"). The
 * legacy `worker://trace` / `subagent://event` channels and their stores
 * (`workerTraceStore`, `subagentStore`) were deleted in Phase 4c; this is now
 * the single source of truth for subagent execution-flow events.
 *
 * Aggregation key is `subagent_run_id` (= framework execution_id, format
 * "{task_id}:{attempt}" for real subagents, "main" for the main-agent
 * synthetic run), which the bridge reads straight off the event — no
 * more temp-allocated dispatch ids.
 */

import { create } from 'zustand';

/** Event variants carried on execution://event with kind="subagent". */
export type SubagentRunEventKind =
  | 'started'
  | 'thinking_started'
  | 'thinking_delta'
  | 'usage' // corresponds to thinking_ended (carries token counts)
  | 'token_delta'
  | 'tool_started'
  | 'tool_completed'
  | 'artifact'
  | 'completed'
  | 'failed'
  | 'cancelled';

/** One raw event on the wire (the bridge emits these as a serde_json::Object). */
export interface ExecutionEvent {
  kind: 'subagent';
  subagent_run_id: string;
  run_id: string;
  agent: string;
  event: SubagentRunEventKind;
  task_id?: string;
  // event-specific fields (any of the below may be absent depending on `event`)
  parent?: string;
  task?: string;
  mode?: string;
  content?: string;
  name?: string; // tool name
  args?: unknown; // tool args
  result?: string; // tool result/error text
  success?: boolean;
  prompt_tokens?: number;
  completion_tokens?: number;
  duration_ms?: number;
  tokens_used?: number;
  iteration_count?: number;
  output?: string;
  error?: string;
  // LLM cache diagnostics (present on `usage` events, emitted per model call)
  message_id?: string;
  model?: string;
  total_tokens?: number;
  cached_prompt_tokens?: number;
  cache_creation_prompt_tokens?: number;
  usage_reported?: boolean;
  [key: string]: unknown;
}

export type SubagentRunStatus = 'running' | 'completed' | 'failed' | 'cancelled';

export interface SubagentRunState {
  subagentRunId: string;
  runId: string;
  taskId?: string;
  agent: string;
  parent?: string;
  task?: string;
  mode?: string;
  status: SubagentRunStatus;
  startedAt: number;
  durationMs?: number;
  tokensUsed?: number;
  iterationCount?: number;
  output?: string;
  error?: string;
  /** Message id that triggered the run (chat message_key); pins this run to a
   * chat message block. Absent for non-chat paths (cron). */
  messageId?: string;
  /** Accumulated LLM usage across all model calls in this run (for cache diagnostics). */
  usageEvents?: ExecutionEvent[];
  /** Append-only event log (thinking/tool/token deltas). Capped to bound memory. */
  events: ExecutionEvent[];
}

interface SubagentRunStore {
  runs: Record<string, SubagentRunState>;
  /** Ingest one `execution://event` payload (kind must be "subagent"). */
  ingest: (ev: ExecutionEvent) => void;
  gc: () => void;
  clear: () => void;
}

const MAX_EVENTS_PER_RUN = 300;

function statusFromEvent(event: SubagentRunEventKind): SubagentRunStatus | null {
  switch (event) {
    case 'started':
      return 'running';
    case 'completed':
      return 'completed';
    case 'failed':
      return 'failed';
    case 'cancelled':
      return 'cancelled';
    default:
      return null;
  }
}

export const useSubagentRunStore = create<SubagentRunStore>((set) => ({
  runs: {},

  ingest: (ev) => {
    set((s) => {
      const id = ev.subagent_run_id;
      const prev = s.runs[id];
      const newStatus = statusFromEvent(ev.event);
      // Lazily create the run on first sight (any event may arrive first in
      // principle, though `started` normally does).
      const run: SubagentRunState = prev ?? {
        subagentRunId: id,
        runId: ev.run_id,
        taskId: ev.task_id,
        agent: ev.agent,
        parent: ev.parent,
        task: ev.task,
        mode: ev.mode,
        status: 'running',
        startedAt: Date.now(),
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
      const usageEvents = ev.event === 'usage' ? [...(run.usageEvents ?? []), ev] : run.usageEvents;
      const next: SubagentRunState = {
        ...run,
        // Preserve any field present on the event (overwrites prev).
        taskId: ev.task_id ?? run.taskId,
        parent: ev.parent ?? run.parent,
        task: ev.task ?? run.task,
        mode: ev.mode ?? run.mode,
        status: newStatus ?? run.status,
        durationMs: ev.duration_ms ?? run.durationMs,
        tokensUsed: ev.tokens_used ?? run.tokensUsed,
        iterationCount: ev.iteration_count ?? run.iterationCount,
        output: ev.output ?? run.output,
        error: ev.error ?? run.error,
        messageId: ev.message_id ?? run.messageId,
        usageEvents,
        events,
      };
      return { runs: { ...s.runs, [id]: next } };
    });
  },

  // GC: drop terminal runs older than 5 minutes (parity with subagentStore).
  gc: () => {
    const cutoff = Date.now() - 5 * 60 * 1000;
    set((s) => {
      const filtered: Record<string, SubagentRunState> = {};
      for (const [k, v] of Object.entries(s.runs)) {
        if (v.status === 'running' || v.startedAt > cutoff) {
          filtered[k] = v;
        }
      }
      return { runs: filtered };
    });
  },

  clear: () => set({ runs: {} }),
}));

// Run GC every 60 seconds when there are entries (parity with subagentStore).
if (typeof window !== 'undefined') {
  setInterval(() => {
    const store = useSubagentRunStore.getState();
    if (Object.keys(store.runs).length > 0) {
      store.gc();
    }
  }, 60_000);
}
