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
  /// True while a plan is awaiting the user's approve/reject decision.
  awaitingApproval: boolean;
  ///Transient error surfaced as a toast/banner.
  error: string | null;
  /// Loading flag for plan generation (the LLM call can take a few seconds).
  generatingPlan: boolean;
  /// Latest route/mode/approval explanation received from plan_ready.
  routeExplanation: RouteExplanation | null;

  // ── Actions ───────────────────────────────────────────────────────────
  refresh: (runId: string) => Promise<void>;
  loadByConversation: (conversationId: string) => Promise<void>;
  generatePlan: (runId: string) => Promise<void>;
  approve: (runId: string, note?: string) => Promise<void>;
  reject: (runId: string, note?: string) => Promise<void>;
  execute: (runId: string) => Promise<void>;
  cancel: (runId: string) => Promise<void>;
  /// Mark that a plan_ready chat event arrived for this run — the panel
  /// should fetch the plan + show approval actions.
  notifyPlanReady: (runId: string, explanation?: Partial<RouteExplanation>) => Promise<void>;
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
  awaitingApproval: false,
  error: null,
  generatingPlan: false,
  routeExplanation: null,

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
          awaitingApproval: false,
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
        awaitingApproval:
          run?.status === ('awaiting_plan_approval' as TaskRunStatus) ||
          run?.status === ('waiting_approval' as TaskRunStatus),
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

  generatePlan: async (runId: string) => {
    set({ generatingPlan: true, error: null });
    try {
      await taskRuntimeApi.generatePlan(runId);
      await get().refresh(runId);
    } catch (e) {
      set({ error: e instanceof Error ? e.message : String(e) });
    } finally {
      set({ generatingPlan: false });
    }
  },

  approve: async (runId: string, note?: string) => {
    try {
      await taskRuntimeApi.approvePlan(runId, note);
      // After approval the run is Ready. Try to auto-launch execution; if it
      // fails (e.g. transient error), the run stays Ready and the panel shows
      // a "执行" button so the user can retry without re-approving.
      try {
        await taskRuntimeApi.executeRun(runId);
      } catch (execErr) {
        set({ error: `批准成功但启动执行失败: ${execErr instanceof Error ? execErr.message : String(execErr)}。可点击"执行"重试。` });
      }
      await get().refresh(runId);
    } catch (e) {
      set({ error: e instanceof Error ? e.message : String(e) });
    }
  },

  reject: async (runId: string, note?: string) => {
    try {
      await taskRuntimeApi.rejectPlan(runId, note);
      await get().refresh(runId);
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

  notifyPlanReady: async (runId: string, explanation?: Partial<RouteExplanation>) => {
    // A plan_ready event arrived — load the run + plan so the panel can
    // render the approval UI.
    set({
      events: [],
      lastSeq: '0',
      routeExplanation: {
        runId,
        plannedWorkers: [],
        suggestedWorkers: [],
        activeSkills: [],
        routeSignals: [],
        classificationSignals: [],
        ...explanation,
      },
    });
    await get().refresh(runId);
    if (!get().activeRun) {
      window.setTimeout(() => {
        void get().refresh(runId);
      }, 500);
    }
  },

  reset: () =>
    set({
      activeRun: null,
      plan: null,
      todos: [],
      events: [],
      artifacts: [],
      lastSeq: '0',
      awaitingApproval: false,
      error: null,
      generatingPlan: false,
      routeExplanation: null,
    }),
}));
