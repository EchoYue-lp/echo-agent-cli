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
let loadGeneration = 0;
let refreshRequestGeneration = 0;

import { create } from 'zustand';
import { taskRuntimeApi, toolExecutionApi } from '../api/endpoints';
import { viewAddress, type ViewAddress } from '../lib/viewAddress';
import { ingestTaskRuntimeSubagentEvents } from './subagentRunStore';
import {
  ingestTaskRuntimeToolExecutions,
  mergeTaskRuntimeToolExecutions,
  taskRuntimeToolExecutions,
  useToolExecutionStore,
} from './toolExecutionStore';
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
  RunContinuationState,
  BackgroundCellState,
  CompletionGateReport,
} from '../generated';

type RunSnapshot = {
  run: TaskRun;
  plan: TaskPlan | null;
  todos: TodoItem[];
  artifacts: RuntimeArtifact[];
  recoveryBlockers: RecoveryBlocker[];
  continuation: RunContinuationState | null;
  backgroundCells: BackgroundCellState[];
  completionGate: CompletionGateReport;
};

async function loadRunSnapshot(workspaceId: string, run: TaskRun): Promise<RunSnapshot> {
  const [plan, todos, artifacts, recoveryBlockers, continuation, backgroundCells, completionGate] =
    await Promise.all([
      taskRuntimeApi.getPlan(workspaceId, run.run_id),
      taskRuntimeApi.listTodos(workspaceId, run.run_id),
      taskRuntimeApi.listArtifacts(workspaceId, run.run_id),
      taskRuntimeApi.listRecoveryBlockers(workspaceId, run.run_id),
      taskRuntimeApi.getContinuation(workspaceId, run.run_id),
      taskRuntimeApi.listBackgroundCells(workspaceId, run.run_id),
      taskRuntimeApi.getCompletionGate(workspaceId, run.run_id),
    ]);
  return {
    run,
    plan,
    todos,
    artifacts,
    recoveryBlockers,
    continuation,
    backgroundCells,
    completionGate,
  };
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
  focusedAddress: ViewAddress | null;
  /// The run the right rail is currently focused on (latest for the active
  /// conversation). Null when no complex task is in flight.
  activeRun: TaskRun | null;
  plan: TaskPlan | null;
  todos: TodoItem[];
  events: RuntimeTaskEvent[];
  artifacts: RuntimeArtifact[];
  recoveryBlockers: RecoveryBlocker[];
  continuation: RunContinuationState | null;
  backgroundCells: BackgroundCellState[];
  completionGate: CompletionGateReport | null;
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
  interruptPrompt: {
    runId: string;
    goal: string;
    newMessage: string;
    resolve: (action: 'continue' | 'edit' | 'cancel_and_start') => Promise<void>;
  } | null;

  /// Polling interval ID — non-null while actively polling a running run.
  pollingInterval: ReturnType<typeof setInterval> | null;

  // ── Actions ───────────────────────────────────────────────────────────
  startPolling: (workspaceId: string, runId: string) => void;
  stopPolling: () => void;
  refresh: (workspaceId: string, runId: string) => Promise<void>;
  loadByConversation: (workspaceId: string, conversationId: string) => Promise<void>;
  cancel: (runId: string) => Promise<void>;
  pause: (runId: string) => Promise<void>;
  updateGoal: (
    runId: string,
    expectedRevision: number,
    goal: string,
    reason: string
  ) => Promise<void>;
  updateContinuationBudgets: (
    runId: string,
    tokenBudget: number | null,
    timeBudgetSeconds: number | null
  ) => Promise<void>;
  openInterruptPrompt: (data: {
    runId: string;
    goal: string;
    newMessage: string;
    resolve: (action: 'continue' | 'edit' | 'cancel_and_start') => Promise<void>;
  }) => void;
  dismissInterruptPrompt: () => void;
  updateTasks: (reason: string, operations: TaskUpdateOperation[]) => Promise<void>;
  insertTask: (afterTaskId: string | null, task: TaskSpec) => Promise<void>;
  skipTask: (taskId: string) => Promise<void>;
  updateTask: (taskId: string, patch: Partial<TaskPatch>) => Promise<void>;
  reorderTasks: (newOrder: string[]) => Promise<void>;
  resumeTaskRun: () => Promise<void>;
  retryBlockedTask: (taskId: string) => Promise<void>;
  resolveRecoveryTask: (taskId: string, decision: 'skip') => Promise<void>;
  skipGoalRequirement: (requirementId: string, reason: string) => Promise<void>;
  reset: () => void;
}

export const useTaskRuntimeStore = create<TaskRuntimeState>((set, get) => ({
  focusedAddress: null,
  activeRun: null,
  plan: null,
  todos: [],
  events: [],
  artifacts: [],
  recoveryBlockers: [],
  continuation: null,
  backgroundCells: [],
  completionGate: null,
  lastSeq: '0',
  error: null,
  generatingPlan: false,
  interruptPrompt: null,
  pollingInterval: null,

  startPolling: (workspaceId: string, runId: string) => {
    const running = ['pending', 'running', 'paused'] as const;
    const { pollingInterval } = get();
    if (pollingInterval !== null) return; // already polling
    const interval = setInterval(() => {
      // P1-7: refresh 含 5 个并行 API 的 Promise.all, 慢网络下可能 >2s。
      // 若上一次未完成, 跳过本次 tick 防止请求堆积 + 乱序 set 覆盖。
      // (用模块级 flag 而非 state, 避免触发多余渲染。)
      if (refreshInFlight) return;
      get()
        .refresh(workspaceId, runId)
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

  refresh: async (workspaceId: string, runId: string) => {
    const generation = loadGeneration;
    const requestGeneration = refreshRequestGeneration + 1;
    refreshRequestGeneration = requestGeneration;
    // P1-7: refreshInFlight 防止 polling 重叠; finally 确保异常时也清除。
    refreshInFlight = true;
    try {
      const [run, events] = await Promise.all([
        taskRuntimeApi.getRun(workspaceId, runId),
        taskRuntimeApi.listEvents(workspaceId, runId, get().lastSeq),
      ]);
      if (generation !== loadGeneration || requestGeneration !== refreshRequestGeneration) {
        return;
      }
      const lastSeq = events.length ? events[events.length - 1].seq : get().lastSeq;
      if (!run) {
        set({
          activeRun: null,
          plan: null,
          todos: [],
          events: [...get().events, ...events].slice(-MAX_EVENTS),
          artifacts: [],
          recoveryBlockers: [],
          continuation: null,
          backgroundCells: [],
          completionGate: null,
          lastSeq,
          error: `TaskRuntime run ${runId} 暂时不可用`,
        });
        return;
      }
      if (run.workspace_id !== workspaceId) {
        throw new Error(`TaskRuntime 工作区不匹配：期望 ${workspaceId}，收到 ${run.workspace_id}`);
      }
      const {
        plan,
        todos,
        artifacts,
        recoveryBlockers,
        continuation,
        backgroundCells,
        completionGate,
      } = await loadRunSnapshot(workspaceId, run);
      if (generation !== loadGeneration || requestGeneration !== refreshRequestGeneration) {
        return;
      }
      ingestTaskRuntimeSubagentEvents(run, plan, events);
      ingestTaskRuntimeToolExecutions(run, events);
      set({
        activeRun: run,
        plan,
        todos,
        // Append-only: merge new events past lastSeq. Cap at 500 to prevent
        // unbounded growth on long-running tasks (events are not rendered).
        events: [...get().events, ...events].slice(-MAX_EVENTS),
        artifacts,
        recoveryBlockers,
        continuation,
        backgroundCells,
        completionGate,
        lastSeq,
        error: null,
      });
    } catch (e) {
      if (generation === loadGeneration && requestGeneration === refreshRequestGeneration) {
        set({ error: e instanceof Error ? e.message : String(e) });
      }
    } finally {
      if (requestGeneration === refreshRequestGeneration) refreshInFlight = false;
    }
  },

  loadByConversation: async (workspaceId: string, conversationId: string) => {
    const generation = loadGeneration + 1;
    loadGeneration = generation;
    refreshRequestGeneration += 1;
    refreshInFlight = false;
    // Loading a conversation is also the lifecycle boundary for polling. The
    // run_started event and app restoration both enter through this method, so
    // leaving polling to the chat command response can strand the panel on its
    // initial Pending snapshot for the entire execution.
    get().stopPolling();
    set({ focusedAddress: viewAddress(workspaceId, conversationId) });
    try {
      const run = await taskRuntimeApi.latestRunForConversation(workspaceId, conversationId);
      if (generation !== loadGeneration) return;
      if (run) {
        if (run.workspace_id !== workspaceId || run.conversation_id !== conversationId) {
          throw new Error('TaskRuntime 返回了不属于当前会话的运行');
        }
        // Reset event cursor when switching runs so we don't cross streams.
        set({ events: [], lastSeq: '0' });
        const [
          {
            plan,
            todos,
            artifacts,
            recoveryBlockers,
            continuation,
            backgroundCells,
            completionGate,
          },
          events,
          persistedTools,
        ] = await Promise.all([
          loadRunSnapshot(workspaceId, run),
          taskRuntimeApi.listEvents(workspaceId, run.run_id, '0'),
          toolExecutionApi.list(workspaceId, run.conversation_id).catch((error) => {
            console.warn('[TaskRuntime] Failed to restore persisted tool executions:', error);
            return [];
          }),
        ]);
        if (generation !== loadGeneration) return;
        const lastSeq = events.length ? events[events.length - 1].seq : '0';
        ingestTaskRuntimeSubagentEvents(run, plan, events);
        useToolExecutionStore
          .getState()
          .hydrateConversation(
            workspaceId,
            run.conversation_id,
            mergeTaskRuntimeToolExecutions(persistedTools, taskRuntimeToolExecutions(run, events))
          );
        set({
          activeRun: run,
          plan,
          todos,
          events: events.slice(-MAX_EVENTS),
          artifacts,
          recoveryBlockers,
          continuation,
          backgroundCells,
          completionGate,
          lastSeq,
          error: null,
        });
        if (run.status === 'pending' || run.status === 'running' || run.status === 'paused') {
          get().startPolling(workspaceId, run.run_id);
        }
      } else {
        set({
          activeRun: null,
          plan: null,
          todos: [],
          events: [],
          artifacts: [],
          recoveryBlockers: [],
          continuation: null,
          backgroundCells: [],
          completionGate: null,
          lastSeq: '0',
          error: null,
        });
      }
    } catch (e) {
      if (generation === loadGeneration) {
        set({ error: e instanceof Error ? e.message : String(e) });
      }
    }
  },

  cancel: async (runId: string) => {
    const workspaceId = get().activeRun?.workspace_id;
    if (!workspaceId) {
      set({ error: '当前任务运行缺少工作区身份' });
      return;
    }
    try {
      await taskRuntimeApi.cancelRun(workspaceId, runId);
      await get().refresh(workspaceId, runId);
    } catch (e) {
      set({ error: e instanceof Error ? e.message : String(e) });
    }
  },

  pause: async (runId: string) => {
    const workspaceId = get().activeRun?.workspace_id;
    if (!workspaceId) {
      set({ error: '当前任务运行缺少工作区身份' });
      return;
    }
    try {
      await taskRuntimeApi.pauseRun(workspaceId, runId);
      await get().refresh(workspaceId, runId);
    } catch (e) {
      set({ error: e instanceof Error ? e.message : String(e) });
    }
  },

  updateGoal: async (runId, expectedRevision, goal, reason) => {
    const workspaceId = get().activeRun?.workspace_id;
    if (!workspaceId) {
      set({ error: '当前任务运行缺少工作区身份' });
      return;
    }
    try {
      await taskRuntimeApi.updateGoal(workspaceId, runId, expectedRevision, goal, reason);
      await get().refresh(workspaceId, runId);
    } catch (e) {
      await get().refresh(workspaceId, runId);
      set({ error: e instanceof Error ? e.message : String(e) });
    }
  },

  updateContinuationBudgets: async (runId, tokenBudget, timeBudgetSeconds) => {
    const workspaceId = get().activeRun?.workspace_id;
    if (!workspaceId) {
      set({ error: '当前任务运行缺少工作区身份' });
      return;
    }
    try {
      await taskRuntimeApi.configureContinuation(
        workspaceId,
        runId,
        tokenBudget,
        timeBudgetSeconds
      );
      await get().refresh(workspaceId, runId);
    } catch (e) {
      set({ error: e instanceof Error ? e.message : String(e) });
    }
  },

  openInterruptPrompt: (data) => set({ interruptPrompt: data }),
  dismissInterruptPrompt: () => set({ interruptPrompt: null }),

  updateTasks: async (reason, operations) => {
    const runId = get().activeRun?.run_id;
    const workspaceId = get().activeRun?.workspace_id;
    const baseRevision = get().plan?.revision;
    if (!workspaceId || !runId || baseRevision === undefined) {
      set({ error: '当前任务图尚未就绪，无法修改' });
      return;
    }
    try {
      const plan = await taskRuntimeApi.updateTasks(workspaceId, runId, {
        base_revision: baseRevision,
        reason,
        operations,
      });
      set({ plan, error: null });
      await get().refresh(workspaceId, runId);
    } catch (e) {
      await get().refresh(workspaceId, runId);
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
    const workspaceId = get().activeRun?.workspace_id;
    if (!workspaceId || !runId) return;
    try {
      await taskRuntimeApi.resumeRun(workspaceId, runId);
      set({ interruptPrompt: null });
      await get().refresh(workspaceId, runId);
    } catch (e) {
      set({ error: e instanceof Error ? e.message : String(e) });
    }
  },
  retryBlockedTask: async (taskId) => {
    const runId = get().activeRun?.run_id;
    const workspaceId = get().activeRun?.workspace_id;
    if (!workspaceId || !runId) return;
    try {
      await taskRuntimeApi.retryBlockedTask(workspaceId, runId, taskId);
      set({ interruptPrompt: null });
      await get().refresh(workspaceId, runId);
    } catch (e) {
      set({ error: e instanceof Error ? e.message : String(e) });
    }
  },
  resolveRecoveryTask: async (taskId, decision) => {
    const runId = get().activeRun?.run_id;
    const workspaceId = get().activeRun?.workspace_id;
    if (!workspaceId || !runId) return;
    try {
      await taskRuntimeApi.resolveRecoveryTask(workspaceId, runId, taskId, decision);
      await get().refresh(workspaceId, runId);
    } catch (e) {
      set({ error: e instanceof Error ? e.message : String(e) });
    }
  },
  skipGoalRequirement: async (requirementId, reason) => {
    const run = get().activeRun;
    if (!run) return;
    try {
      const completionGate = await taskRuntimeApi.skipGoalRequirement(
        run.workspace_id,
        run.run_id,
        run.goal_revision,
        requirementId,
        reason
      );
      set({ completionGate, error: null });
      await get().refresh(run.workspace_id, run.run_id);
    } catch (e) {
      await get().refresh(run.workspace_id, run.run_id);
      set({ error: e instanceof Error ? e.message : String(e) });
    }
  },

  reset: () => {
    loadGeneration += 1;
    refreshRequestGeneration += 1;
    refreshInFlight = false;
    get().stopPolling();
    set({
      focusedAddress: null,
      activeRun: null,
      plan: null,
      todos: [],
      events: [],
      artifacts: [],
      recoveryBlockers: [],
      continuation: null,
      backgroundCells: [],
      completionGate: null,
      lastSeq: '0',
      error: null,
      generatingPlan: false,
      interruptPrompt: null,
    });
  },
}));
