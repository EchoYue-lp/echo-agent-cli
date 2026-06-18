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
  notifyPlanReady: (runId: string) => Promise<void>;
  reset: () => void;
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
        set({ events: [], lastSeq: '0' });
        await get().refresh(run.run_id);
      } else {
        set({ activeRun: null, plan: null, todos: [], events: [], artifacts: [], lastSeq: '0' });
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

  notifyPlanReady: async (runId: string) => {
    // A plan_ready event arrived — load the run + plan so the panel can
    // render the approval UI.
    set({ events: [], lastSeq: '0' });
    await get().refresh(runId);
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
    }),
}));
