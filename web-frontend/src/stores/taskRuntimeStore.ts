//! TaskRuntime store — the GUI's view of complex-task runs.
//!
//! Holds the active run, its plan, todos, artifacts, and event feed, keyed by
//! run_id. Polling is driven by the component (RightRail panel) via
//! `refresh(runId)`; the store just holds state and exposes mutators.
//!
//! This is deliberately separate from `chatStore` (which is a per-message
//! streaming store) — a TaskRuntime run outlives any single chat turn and
//! survives page refresh via the canonical file-backed store on the backend.

/// Maximum number of events retained in-memory. Events are not rendered
/// (only plan/todos/artifacts are), so this cap prevents unbounded growth
/// on long-running complex tasks without impacting the user-facing UI.
const MAX_EVENTS = 500;

/// P1-7: 防止 polling 的 refresh 重叠。模块级而非 state 字段, 避免触发渲染。
let refreshInFlight = false;

import { create } from 'zustand';
import { taskRuntimeApi } from '../api/endpoints';
import type {
  TaskRun,
  TaskPlan,
  TodoItem,
  RuntimeTaskEvent,
  RuntimeArtifact,
  RecoveryBlocker,
} from '../generated';

type RunSnapshot = {
  run: TaskRun;
  plan: TaskPlan | null;
  todos: TodoItem[];
  artifacts: RuntimeArtifact[];
};

function runTime(value: string): number {
  const time = Date.parse(value);
  return Number.isFinite(time) ? time : 0;
}

async function loadRunSnapshot(run: TaskRun): Promise<RunSnapshot> {
  const [plan, todos, artifacts] = await Promise.all([
    taskRuntimeApi.getPlan(run.run_id),
    taskRuntimeApi.listTodos(run.run_id),
    taskRuntimeApi.listArtifacts(run.run_id),
  ]);
  return { run, plan, todos, artifacts };
}

async function loadConversationRunGroup(conversationId: string, focusedRun: TaskRun) {
  const allRuns = await taskRuntimeApi.listRuns();
  const groupRuns = allRuns
    .filter(
      (run) =>
        run.conversation_id === conversationId && run.root_message_id === focusedRun.root_message_id
    )
    .sort((a, b) => runTime(a.created_at) - runTime(b.created_at));
  const runs = groupRuns.length ? groupRuns : [focusedRun];
  const snapshots = await Promise.all(runs.map(loadRunSnapshot));
  const recoveryBlockers = await taskRuntimeApi.listRecoveryBlockers(focusedRun.run_id);
  const basePlan = snapshots.find((snapshot) => snapshot.plan)?.plan ?? null;
  const plan = basePlan
    ? {
        ...basePlan,
        run_id: focusedRun.run_id,
        goal: focusedRun.goal,
        tasks: snapshots.flatMap((snapshot) => snapshot.plan?.tasks ?? []),
      }
    : null;
  return {
    plan,
    todos: snapshots.flatMap((snapshot) => snapshot.todos),
    artifacts: snapshots.flatMap((snapshot) => snapshot.artifacts),
    recoveryBlockers,
  };
}

export interface TaskRuntimeState {
  /// The run the right rail is currently focused on (latest for the active
  /// conversation). Null when no complex task is in flight.
  activeRun: TaskRun | null;
  plan: TaskPlan | null;
  todos: TodoItem[];
  events: RuntimeTaskEvent[];
  artifacts: RuntimeArtifact[];
  recoveryBlockers: RecoveryBlocker[];
  /// Highest event seq we've already ingested (string per the seq-as-string
  /// transport contract). Used for incremental polling.
  lastSeq: string;
  ///Transient error surfaced as a toast/banner.
  error: string | null;
  /// Loading flag for plan generation (the LLM call can take a few seconds).
  generatingPlan: boolean;
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
  cancel: (runId: string) => Promise<void>;
  pause: (runId: string) => Promise<void>;
  openInterruptPrompt: (data: { runId: string; goal: string; newMessage: string }) => void;
  dismissInterruptPrompt: () => void;
  // Dynamic task operations (Phase 2).
  insertTask: (afterTaskId: string | null, task: Record<string, unknown>) => Promise<void>;
  removeTask: (taskId: string) => Promise<void>;
  updateTask: (taskId: string, patch: Record<string, unknown>) => Promise<void>;
  reorderTasks: (newOrder: string[]) => Promise<void>;
  resumeTaskRun: () => Promise<void>;
  retryBlockedTask: (taskId: string) => Promise<void>;
  resolveRecoveryTask: (taskId: string, decision: 'retry' | 'skip') => Promise<void>;
  reset: () => void;
}

export const useTaskRuntimeStore = create<TaskRuntimeState>((set, get) => ({
  activeRun: null,
  plan: null,
  todos: [],
  events: [],
  artifacts: [],
  recoveryBlockers: [],
  lastSeq: '0',
  error: null,
  generatingPlan: false,
  interruptPrompt: null,
  pollingInterval: null,

  startPolling: (runId: string) => {
    const running = ['pending', 'running', 'paused'] as const;
    const { pollingInterval } = get();
    if (pollingInterval !== null) return; // already polling
    const interval = setInterval(() => {
      // P1-7: refresh 含 5 个并行 API 的 Promise.all, 慢网络下可能 >2s。
      // 若上一次未完成, 跳过本次 tick 防止请求堆积 + 乱序 set 覆盖。
      // (用模块级 flag 而非 state, 避免触发多余渲染。)
      if (refreshInFlight) return;
      get()
        .refresh(runId)
        .then(() => {
          const status = get().activeRun?.status;
          if (status && !running.includes(status as (typeof running)[number])) {
            get().stopPolling();
          }
        })
        .catch(() => {
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
    // P1-7: refreshInFlight 防止 polling 重叠; finally 确保异常时也清除。
    refreshInFlight = true;
    try {
      const [run, events] = await Promise.all([
        taskRuntimeApi.getRun(runId),
        taskRuntimeApi.listEvents(runId, get().lastSeq),
      ]);
      const lastSeq = events.length ? events[events.length - 1].seq : get().lastSeq;
      if (!run) {
        set({
          activeRun: null,
          plan: null,
          todos: [],
          events: [...get().events, ...events].slice(-MAX_EVENTS),
          artifacts: [],
          lastSeq,
          error: `TaskRuntime run ${runId} 暂时不可用`,
        });
        return;
      }
      const { plan, todos, artifacts, recoveryBlockers } = await loadConversationRunGroup(
        run.conversation_id,
        run
      );
      set({
        activeRun: run,
        plan,
        todos,
        // Append-only: merge new events past lastSeq. Cap at 500 to prevent
        // unbounded growth on long-running tasks (events are not rendered).
        events: [...get().events, ...events].slice(-MAX_EVENTS),
        artifacts,
        recoveryBlockers,
        lastSeq,
        error: null,
      });
    } catch (e) {
      set({ error: e instanceof Error ? e.message : String(e) });
    } finally {
      refreshInFlight = false;
    }
  },

  loadByConversation: async (conversationId: string) => {
    try {
      const run = await taskRuntimeApi.latestRunForConversation(conversationId);
      if (run) {
        // Reset event cursor when switching runs so we don't cross streams.
        set({ events: [], lastSeq: '0' });
        const { plan, todos, artifacts, recoveryBlockers } = await loadConversationRunGroup(
          conversationId,
          run
        );
        set({
          activeRun: run,
          plan,
          todos,
          events: [],
          artifacts,
          recoveryBlockers,
          lastSeq: '0',
          error: null,
        });
      } else {
        set({
          activeRun: null,
          plan: null,
          todos: [],
          events: [],
          artifacts: [],
          recoveryBlockers: [],
          lastSeq: '0',
        });
      }
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

  pause: async (runId: string) => {
    try {
      await taskRuntimeApi.pauseRun(runId);
      await get().refresh(runId);
    } catch (e) {
      set({ error: e instanceof Error ? e.message : String(e) });
    }
  },

  openInterruptPrompt: (data) => set({ interruptPrompt: data }),
  dismissInterruptPrompt: () => set({ interruptPrompt: null }),

  insertTask: async (afterTaskId, task) => {
    const runId = get().activeRun?.run_id;
    if (!runId) return;
    await taskRuntimeApi.insertTask(
      runId,
      afterTaskId,
      task as unknown as import('../generated').PlanTask
    );
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
    try {
      await taskRuntimeApi.resumeRun(runId);
      set({ interruptPrompt: null });
      await get().refresh(runId);
    } catch (e) {
      set({ error: e instanceof Error ? e.message : String(e) });
    }
  },
  retryBlockedTask: async (taskId) => {
    const runId = get().activeRun?.run_id;
    if (!runId) return;
    try {
      await taskRuntimeApi.retryBlockedTask(runId, taskId);
      set({ interruptPrompt: null });
      await get().refresh(runId);
    } catch (e) {
      set({ error: e instanceof Error ? e.message : String(e) });
    }
  },
  resolveRecoveryTask: async (taskId, decision) => {
    const runId = get().activeRun?.run_id;
    if (!runId) return;
    try {
      await taskRuntimeApi.resolveRecoveryTask(runId, taskId, decision);
      await get().refresh(runId);
    } catch (e) {
      set({ error: e instanceof Error ? e.message : String(e) });
    }
  },

  reset: () => {
    get().stopPolling();
    set({
      activeRun: null,
      plan: null,
      todos: [],
      events: [],
      artifacts: [],
      recoveryBlockers: [],
      lastSeq: '0',
      error: null,
      generatingPlan: false,
      interruptPrompt: null,
    });
  },
}));
