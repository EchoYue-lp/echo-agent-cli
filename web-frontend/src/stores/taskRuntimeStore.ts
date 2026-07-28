//! TaskRuntime store — the GUI's view of complex-task runs.
//!
//! Holds the active run, its plan, todos, artifacts, and event feed, keyed by
//! run_id. Loading an active conversation owns the polling lifecycle so every
//! surface observes the persisted terminal snapshot.
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
  TaskSpec,
  TaskPatch,
  TaskUpdateOperation,
} from '../generated';

type RunSnapshot = {
  run: TaskRun;
  plan: TaskPlan | null;
  todos: TodoItem[];
  artifacts: RuntimeArtifact[];
  recoveryBlockers: RecoveryBlocker[];
};

async function loadRunSnapshot(run: TaskRun): Promise<RunSnapshot> {
  const [plan, todos, artifacts, recoveryBlockers] = await Promise.all([
    taskRuntimeApi.getPlan(run.run_id),
    taskRuntimeApi.listTodos(run.run_id),
    taskRuntimeApi.listArtifacts(run.run_id),
    taskRuntimeApi.listRecoveryBlockers(run.run_id),
  ]);
  return { run, plan, todos, artifacts, recoveryBlockers };
}

function completeTaskPatch(patch: Partial<TaskPatch>): TaskPatch {
  return {
    title: patch.title ?? null,
    description: patch.description ?? null,
    kind: patch.kind ?? null,
    agent_role: patch.agent_role ?? null,
    depends_on: patch.depends_on ?? null,
    files: patch.files ?? null,
    allowed_tools: patch.allowed_tools ?? null,
    required_artifacts: patch.required_artifacts ?? null,
    execution_checks: patch.execution_checks ?? null,
    acceptance_criteria: patch.acceptance_criteria ?? null,
    max_retries: patch.max_retries ?? null,
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
  updateTasks: (reason: string, operations: TaskUpdateOperation[]) => Promise<void>;
  insertTask: (afterTaskId: string | null, task: TaskSpec) => Promise<void>;
  skipTask: (taskId: string) => Promise<void>;
  updateTask: (taskId: string, patch: Partial<TaskPatch>) => Promise<void>;
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
      const { plan, todos, artifacts, recoveryBlockers } = await loadRunSnapshot(run);
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
    // Loading a conversation is also the lifecycle boundary for polling. The
    // run_started event and app restoration both enter through this method, so
    // leaving polling to the chat command response can strand the panel on its
    // initial Pending snapshot for the entire execution.
    get().stopPolling();
    try {
      const run = await taskRuntimeApi.latestRunForConversation(conversationId);
      if (run) {
        // Reset event cursor when switching runs so we don't cross streams.
        set({ events: [], lastSeq: '0' });
        const { plan, todos, artifacts, recoveryBlockers } = await loadRunSnapshot(run);
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
        if (run.status === 'pending' || run.status === 'running' || run.status === 'paused') {
          get().startPolling(run.run_id);
        }
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

  updateTasks: async (reason, operations) => {
    const runId = get().activeRun?.run_id;
    const baseRevision = get().plan?.revision;
    if (!runId || baseRevision === undefined) {
      set({ error: '当前任务图尚未就绪，无法修改' });
      return;
    }
    try {
      const plan = await taskRuntimeApi.updateTasks(runId, {
        base_revision: baseRevision,
        reason,
        operations,
      });
      set({ plan, error: null });
      await get().refresh(runId);
    } catch (e) {
      await get().refresh(runId);
      set({ error: e instanceof Error ? e.message : String(e) });
    }
  },
  insertTask: async (afterTaskId, task) => {
    await get().updateTasks(`新增任务：${task.title}`, [
      { op: 'insert', after_task_id: afterTaskId, task },
    ]);
  },
  skipTask: async (taskId) => {
    await get().updateTasks(`跳过任务：${taskId}`, [{ op: 'skip', task_id: taskId }]);
  },
  updateTask: async (taskId, patch) => {
    await get().updateTasks(`更新任务：${taskId}`, [
      { op: 'update', task_id: taskId, patch: completeTaskPatch(patch) },
    ]);
  },
  reorderTasks: async (newOrder) => {
    await get().updateTasks('调整任务顺序', [{ op: 'reorder', task_ids: newOrder }]);
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
