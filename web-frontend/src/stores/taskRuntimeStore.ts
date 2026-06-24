//! TaskRuntime store — the GUI's view of complex-task runs.
//!
//! Holds the active run, its plan, todos, artifacts, and event feed, keyed by
//! run_id. Polling is driven by the component (RightRail panel) via
//! `refresh(runId)`; the store just holds state and exposes mutators.
//!
//! This is deliberately separate from `chatStore` (which is a per-message
//! streaming store) — a TaskRuntime run outlives any single chat turn and
//! survives page refresh via the canonical SQLite store on the backend.

/// Maximum number of events retained in-memory. Events are not rendered
/// (only plan/todos/artifacts are), so this cap prevents unbounded growth
/// on long-running complex tasks without impacting the user-facing UI.
const MAX_EVENTS = 500;

import { create } from 'zustand';
import { taskRuntimeApi } from '../api/endpoints';
import type {
  TaskRun,
  TaskPlan,
  TodoItem,
  RuntimeTaskEvent,
  RuntimeArtifact,
  TaskRunStatus,
} from '../generated';
import { useWorkerTraceStore, type WorkerTraceEvent } from './workerTraceStore';

export interface RouteExplanation {
  runId: string;
  goal?: string;
  domainProfile?: string;
  route?: string;
  interactionMode?: string;
  permissionMode?: string;
  approvalPolicy?: string;
  routeReason?: string;
  confidence?: number;
  autoExecute?: boolean;
  plannedWorkers: string[];
  suggestedWorkers: string[];
  activeSkills: string[];
  routeSignals: string[];
  classificationSignals: string[];
}

export interface TaskRuntimeState {
  /// The run the right rail is currently focused on (latest for the active
  /// conversation). Null when no complex task is in flight.
  activeRun: TaskRun | null;
  plan: TaskPlan | null;
  todos: TodoItem[];
  events: RuntimeTaskEvent[];
  artifacts: RuntimeArtifact[];
  /// Highest event seq we've already ingested (string per the seq-as-string
  /// transport contract). Used for incremental polling.
  lastSeq: string;
  ///Transient error surfaced as a toast/banner.
  error: string | null;
  /// Loading flag for plan generation (the LLM call can take a few seconds).
  generatingPlan: boolean;
  /// Latest route/mode/approval explanation received from plan_ready.
  routeExplanation: RouteExplanation | null;
  /// Interrupt prompt: set when a new message arrives while a run is
  /// in-progress. The GUI shows a dialog letting the user choose:
  /// resume / edit-and-resume / abandon.
  interruptPrompt: { runId: string; goal: string; newMessage: string } | null;

  /// Polling interval ID — non-null while actively polling a running run.
  pollingInterval: ReturnType<typeof setInterval> | null;

  // ── Actions ───────────────────────────────────────────────────────────
  startPolling: (runId: string) => void;
  stopPolling: () => void;
  refresh: (runId: string) => Promise<void>;
  loadByConversation: (conversationId: string) => Promise<void>;
  execute: (runId: string) => Promise<void>;
  cancel: (runId: string) => Promise<void>;
  openInterruptPrompt: (data: { runId: string; goal: string; newMessage: string }) => void;
  dismissInterruptPrompt: () => void;
  updateRunStatus: (status: string) => void;
  // Dynamic task operations (Phase 2).
  insertTask: (afterTaskId: string | null, task: Record<string, unknown>) => Promise<void>;
  removeTask: (taskId: string) => Promise<void>;
  updateTask: (taskId: string, patch: Record<string, unknown>) => Promise<void>;
  reorderTasks: (newOrder: string[]) => Promise<void>;
  resumeTaskRun: () => Promise<void>;
  reset: () => void;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return Boolean(value && typeof value === 'object' && !Array.isArray(value));
}

function stringField(record: Record<string, unknown>, key: string): string | undefined {
  const value = record[key];
  return typeof value === 'string' && value.length > 0 ? value : undefined;
}

function replayPersistedWorkerUsage(events: RuntimeTaskEvent[]) {
  const append = useWorkerTraceStore.getState().append;
  for (const event of events) {
    if (event.event_type !== 'worker_llm_usage' || !isRecord(event.payload)) continue;

    const usage = isRecord(event.payload.usage) ? event.payload.usage : event.payload;
    const workerId =
      stringField(event.payload, 'worker_id') ?? event.step_id ?? event.task_id ?? undefined;
    if (!workerId) continue;

    const workerEvent: WorkerTraceEvent = {
      event_id: `runtime:${event.run_id}:${event.seq}`,
      run_id: event.run_id,
      worker_id: workerId,
      parent_worker_id: null,
      agent_name: stringField(event.payload, 'agent_name') ?? null,
      title: stringField(event.payload, 'title') ?? null,
      task: null,
      event_type: 'worker_llm_usage',
      payload: usage,
      timestamp: event.timestamp,
    };
    append(workerEvent);
  }
}

export const useTaskRuntimeStore = create<TaskRuntimeState>((set, get) => ({
  activeRun: null,
  plan: null,
  todos: [],
  events: [],
  artifacts: [],
  lastSeq: '0',
  error: null,
  generatingPlan: false,
  routeExplanation: null,
  interruptPrompt: null,
  pollingInterval: null,

  startPolling: (runId: string) => {
    const running = ['pending', 'running', 'paused'] as const;
    const { pollingInterval } = get();
    if (pollingInterval !== null) return; // already polling
    const interval = setInterval(() => {
      get().refresh(runId).then(() => {
        const status = get().activeRun?.status;
        if (status && !running.includes(status as typeof running[number])) {
          get().stopPolling();
        }
      }).catch(() => {
        // refresh errors are handled inside refresh()
      });
    }, 2000);
    set({ pollingInterval: interval });
  },

  stopPolling: () => {
    const { pollingInterval } = get();
    if (pollingInterval !== null) {
      clearInterval(pollingInterval);
      set({ pollingInterval: null });
    }
  },

  refresh: async (runId: string) => {
    try {
      const [run, plan, todos, events, artifacts] = await Promise.all([
        taskRuntimeApi.getRun(runId),
        taskRuntimeApi.getPlan(runId),
        taskRuntimeApi.listTodos(runId),
        taskRuntimeApi.listEvents(runId, get().lastSeq),
        taskRuntimeApi.listArtifacts(runId),
      ]);
      const lastSeq = events.length
        ? events[events.length - 1].seq
        : get().lastSeq;
      replayPersistedWorkerUsage(events);
      if (!run) {
        set({
          activeRun: null,
          plan,
          todos,
          events: [...get().events, ...events].slice(-MAX_EVENTS),
          artifacts,
          lastSeq,
          error: `TaskRuntime run ${runId} 暂时不可用`,
        });
        return;
      }
      set({
        activeRun: run,
        plan,
        todos,
        // Append-only: merge new events past lastSeq. Cap at 500 to prevent
        // unbounded growth on long-running tasks (events are not rendered).
        events: [...get().events, ...events].slice(-MAX_EVENTS),
        artifacts,
        lastSeq,
        error: null,
      });
    } catch (e) {
      set({ error: e instanceof Error ? e.message : String(e) });
    }
  },

  loadByConversation: async (conversationId: string) => {
    try {
      const run = await taskRuntimeApi.latestRunForConversation(conversationId);
      if (run) {
        // Reset event cursor when switching runs so we don't cross streams.
        set({ events: [], lastSeq: '0', routeExplanation: null });
        await get().refresh(run.run_id);
      } else {
        set({ activeRun: null, plan: null, todos: [], events: [], artifacts: [], lastSeq: '0', routeExplanation: null });
      }
    } catch (e) {
      set({ error: e instanceof Error ? e.message : String(e) });
    }
  },



  execute: async (runId: string) => {
    try {
      await taskRuntimeApi.executeRun(runId);
      await get().refresh(runId);
    } catch (e) {
      set({ error: e instanceof Error ? e.message : String(e) });
    }
  },

  cancel: async (runId: string) => {
    try {
      await taskRuntimeApi.cancelRun(runId);
      await get().refresh(runId);
    } catch (e) {
      set({ error: e instanceof Error ? e.message : String(e) });
    }
  },

  openInterruptPrompt: (data) => set({ interruptPrompt: data }),
  dismissInterruptPrompt: () => set({ interruptPrompt: null }),

  updateRunStatus: (status: string) => {
    const run = get().activeRun;
    if (run) {
      set({ activeRun: { ...run, status: status as TaskRunStatus } });
      if (['completed', 'failed', 'cancelled'].includes(status)) {
        get().stopPolling?.();
      }
    }
  },

  insertTask: async (afterTaskId, task) => {
    const runId = get().activeRun?.run_id;
    if (!runId) return;
    await taskRuntimeApi.insertTask(runId, afterTaskId, task as unknown as import('../generated').PlanTask);
    await get().refresh(runId);
  },
  removeTask: async (taskId) => {
    const runId = get().activeRun?.run_id;
    if (!runId) return;
    await taskRuntimeApi.removeTask(runId, taskId);
    await get().refresh(runId);
  },
  updateTask: async (taskId, patch) => {
    const runId = get().activeRun?.run_id;
    if (!runId) return;
    await taskRuntimeApi.updateTask(runId, taskId, patch);
    await get().refresh(runId);
  },
  reorderTasks: async (newOrder) => {
    const runId = get().activeRun?.run_id;
    if (!runId) return;
    await taskRuntimeApi.reorderTasks(runId, newOrder);
    await get().refresh(runId);
  },
  resumeTaskRun: async () => {
    const runId = get().activeRun?.run_id;
    if (!runId) return;
    await taskRuntimeApi.resumeRun(runId);
    set({ interruptPrompt: null });
    await get().refresh(runId);
  },

  reset: () => {
    get().stopPolling();
    set({
      activeRun: null,
      plan: null,
      todos: [],
      events: [],
      artifacts: [],
      lastSeq: '0',
      error: null,
      generatingPlan: false,
      routeExplanation: null,
      interruptPrompt: null,
    });
  },
}));
