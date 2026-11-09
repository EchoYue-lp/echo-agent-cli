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
let inventoryRequestGeneration = 0;
let historyRequestGeneration = 0;
let historyRefreshRequestGeneration = 0;
let eventRequestGeneration = 0;
let pollingTickInFlight = false;

import { create } from 'zustand';
import {
  taskRuntimeApi,
  toolExecutionApi,
  type ProductDataScope,
  type TaskEventPage,
} from '../api/endpoints';
import { sameProductDataScope } from '../lib/productDataScope';
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
  PlanRevision,
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
  TaskRunControlReceipt,
  TaskRunResumeReceipt,
  TaskRetryReceipt,
} from '../generated';

type RunSnapshot = {
  plan: PlanRevision | null;
  todos: TodoItem[];
  artifacts: RuntimeArtifact[];
  recoveryBlockers: RecoveryBlocker[];
  continuation: RunContinuationState | null;
  backgroundCells: BackgroundCellState[];
  completionGate: CompletionGateReport;
};

async function loadRunSnapshot(
  workspaceId: string,
  run: TaskRun,
  generation?: string
): Promise<RunSnapshot> {
  const [plan, todos, artifacts, recoveryBlockers, continuation, backgroundCells, completionGate] =
    await Promise.all([
      taskRuntimeApi.getPlan(workspaceId, run.run_id, generation),
      taskRuntimeApi.listTodos(workspaceId, run.run_id, generation),
      taskRuntimeApi.listArtifacts(workspaceId, run.run_id, generation),
      taskRuntimeApi.listRecoveryBlockers(workspaceId, run.run_id, generation),
      taskRuntimeApi.getContinuation(workspaceId, run.run_id, generation),
      taskRuntimeApi.listBackgroundCells(workspaceId, run.run_id, generation),
      taskRuntimeApi.getCompletionGate(workspaceId, run.run_id, generation),
    ]);
  return {
    plan,
    todos,
    artifacts,
    recoveryBlockers,
    continuation,
    backgroundCells,
    completionGate,
  };
}

function checkEventPage(page: TaskEventPage, runId: string, after: string): TaskEventPage {
  if (!/^\d+$/.test(after) || page.events.length > 256)
    throw new Error('执行记录分页位置或大小无效');
  let sequence = BigInt(after);
  for (const event of page.events) {
    sequence += 1n;
    if (event.run_id !== runId || !/^\d+$/.test(event.seq) || BigInt(event.seq) !== sequence) {
      throw new Error('执行记录存在缺口或属于其它运行，请刷新');
    }
  }
  if (
    page.next_after_sequence !== sequence.toString() ||
    (page.has_more && page.events.length === 0)
  ) {
    throw new Error('执行记录分页未提供有效的后续位置');
  }
  return page;
}

async function initialRunData(
  workspaceId: string,
  run: TaskRun,
  generation?: string,
  restoreChatTools = true
) {
  const [snapshot, page, persistedTools] = await Promise.all([
    loadRunSnapshot(workspaceId, run, generation),
    taskRuntimeApi.listEvents(workspaceId, run.run_id, '0', generation),
    restoreChatTools
      ? toolExecutionApi.list(workspaceId, run.conversation_id).catch((error) => {
          console.warn('[TaskRuntime] Failed to restore persisted tool executions:', error);
          return [];
        })
      : Promise.resolve([]),
  ]);
  return { snapshot, page: checkEventPage(page, run.run_id, '0'), persistedTools };
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

export type TaskRuntimeView = Pick<
  TaskRuntimeState,
  | 'activeRun'
  | 'plan'
  | 'todos'
  | 'events'
  | 'artifacts'
  | 'recoveryBlockers'
  | 'continuation'
  | 'backgroundCells'
  | 'completionGate'
  | 'lastSeq'
  | 'hasMoreEvents'
  | 'error'
>;

export type RunAddress = Pick<TaskRun, 'workspace_id' | 'run_id'> & {
  workspace_generation: string;
};

/** One bounded historical presentation cache; ordinary chat consumers retain activeRun. */
export function taskRuntimePresentation(state: TaskRuntimeState): TaskRuntimeView {
  if (state.selectedRunId === state.activeRun?.run_id) return state;
  return state.historySnapshot && state.selectedRunId === state.historySnapshot.activeRun?.run_id
    ? state.historySnapshot
    : state;
}

function viewForRun(state: TaskRuntimeState, runId?: string): TaskRuntimeView | null {
  if (!runId || state.activeRun?.run_id === runId) return state;
  return state.historySnapshot?.activeRun?.run_id === runId ? state.historySnapshot : null;
}

/** 同一作用域/run的两份现有投影以後端版本拒绝回退，不用时间戳推断新旧。 */
function hasNewerRunProjection(state: TaskRuntimeState, run: TaskRun, plan: PlanRevision | null) {
  return [state, state.historySnapshot].some(
    (view) =>
      view?.activeRun?.run_id === run.run_id &&
      view.activeRun.workspace_id === run.workspace_id &&
      (view.activeRun.goal_revision > run.goal_revision ||
        (view.plan?.run_id === run.run_id && view.plan.revision > (plan?.revision ?? 0)))
  );
}

/** Select/refresh a presentation after the original API settles; this owns no execution. */
async function runAction<T>(
  get: () => TaskRuntimeState,
  set: (patch: Partial<TaskRuntimeState>) => void,
  target: string | RunAddress | undefined,
  operation: (address: RunAddress, view: TaskRuntimeView | null) => Promise<T>,
  refreshOnError = false,
  apply?: (result: T, current: TaskRuntimeView) => Partial<TaskRuntimeView>,
  throwOnError = false,
  canonicalGoalReceipt = false
): Promise<T | undefined> {
  const before = get();
  const view = viewForRun(before, typeof target === 'string' ? target : target?.run_id);
  const run = typeof target === 'object' ? target : view?.activeRun;
  if (!run) return undefined;
  const capturedScope =
    typeof target === 'object'
      ? { workspaceId: target.workspace_id, workspaceGeneration: target.workspace_generation }
      : before.taskScope;
  if (
    !capturedScope ||
    capturedScope.workspaceId !== run.workspace_id ||
    !capturedScope.workspaceGeneration
  ) {
    set({ error: '任务范围已失效，请重新载入后重试' });
    return undefined;
  }
  const address: RunAddress = { ...run, workspace_generation: capturedScope.workspaceGeneration };
  const chatVersion = loadGeneration;
  const historyVersion = historyRequestGeneration;
  let appliedGoalReceipt = false;
  const stillOwned = (acceptedGoal = false) => {
    const state = get();
    return (
      !!state.taskScope &&
      sameProductDataScope(state.taskScope, capturedScope) &&
      ((state.activeRun?.run_id === address.run_id &&
        state.activeRun.workspace_id === address.workspace_id &&
        loadGeneration === chatVersion &&
        (historyRequestGeneration === historyVersion || acceptedGoal)) ||
        (state.historySnapshot?.activeRun?.run_id === address.run_id &&
          state.historySnapshot.activeRun.workspace_id === address.workspace_id &&
          (historyRequestGeneration === historyVersion || acceptedGoal)))
    );
  };
  const errorForTarget = (error: string | null) => {
    if (!stillOwned(appliedGoalReceipt)) return;
    const state = get();
    if (
      state.activeRun?.run_id === address.run_id &&
      state.activeRun.workspace_id === address.workspace_id
    )
      set({
        error,
        ...(appliedGoalReceipt &&
        error &&
        state.historySnapshot?.activeRun?.run_id === address.run_id &&
        state.historySnapshot.activeRun.workspace_id === address.workspace_id
          ? { historySnapshot: { ...state.historySnapshot, error } }
          : {}),
      });
    else if (state.historySnapshot) set({ historySnapshot: { ...state.historySnapshot, error } });
  };
  errorForTarget(null);
  try {
    const result = await operation(address, view);
    if (
      result &&
      typeof result === 'object' &&
      'run_id' in result &&
      result.run_id !== address.run_id
    ) {
      throw new Error('后端回执不属于原运行');
    }
    // 浏览切换退役旧操作的呈现owner；只有已成功提交的Goal回执仍需更新同run规范目标。
    if (stillOwned(canonicalGoalReceipt)) {
      if (apply) {
        const state = get();
        const isChat =
          state.activeRun?.run_id === address.run_id &&
          state.activeRun.workspace_id === address.workspace_id;
        const current = isChat ? state : state.historySnapshot;
        if (!current) return result;
        const patch = apply(result, current);
        appliedGoalReceipt = !!patch.activeRun;
        if (appliedGoalReceipt) {
          const history =
            state.historySnapshot?.activeRun?.run_id === address.run_id &&
            state.historySnapshot.activeRun.workspace_id === address.workspace_id
              ? state.historySnapshot
              : isChat && state.selectedRunId === address.run_id
                ? {
                    activeRun: state.activeRun,
                    plan: state.plan,
                    todos: state.todos,
                    events: state.events,
                    artifacts: state.artifacts,
                    recoveryBlockers: state.recoveryBlockers,
                    continuation: state.continuation,
                    backgroundCells: state.backgroundCells,
                    completionGate: state.completionGate,
                    lastSeq: state.lastSeq,
                    hasMoreEvents: state.hasMoreEvents,
                    error: state.error,
                  }
                : null;
          // Goal CAS 的真实回执不因后续读取失败而失效；两份匹配视图仍保留各自事件游标。
          set({
            ...(isChat ? patch : {}),
            ...(history ? { historySnapshot: { ...history, ...apply(result, history) } } : {}),
            ...(state.selectedRunId === address.run_id ? { loadingRun: false } : {}),
          });
          const committed = viewForRun(get(), address.run_id)?.activeRun;
          if (committed && ['pending', 'running', 'paused'].includes(committed.status))
            get().startPolling(committed.workspace_id, committed.run_id);
        } else if (isChat) set(patch);
        else if (state.historySnapshot)
          set({ historySnapshot: { ...state.historySnapshot, ...patch } });
      }
      await get().refresh(address.workspace_id, address.run_id);
      const refreshed = viewForRun(get(), address.run_id);
      if (appliedGoalReceipt && refreshed?.error) errorForTarget(refreshed.error);
    }
    return result;
  } catch (error) {
    if (refreshOnError && stillOwned(appliedGoalReceipt))
      await get().refresh(address.workspace_id, address.run_id);
    const message = error instanceof Error ? error.message : String(error);
    errorForTarget(message);
    if (throwOnError) throw error instanceof Error ? error : new Error(message);
    return undefined;
  }
}

export interface TaskRuntimeState {
  taskScope: ProductDataScope | null;
  historySnapshot: TaskRuntimeView | null;
  historyError: string | null;
  refreshHistory: () => Promise<void>;
  selectedRunId: string | null;
  runPage: TaskRun[];
  runStatuses: TaskRun['status'][];
  nextRunContinuation: string | null;
  loadingRuns: boolean;
  runListError: string | null;
  loadingRun: boolean;
  hasMoreEvents: boolean;
  loadingEvents: boolean;
  bindScope: (scope: ProductDataScope) => void;
  loadRuns: (
    scope: ProductDataScope,
    statuses?: TaskRun['status'][],
    more?: boolean
  ) => Promise<void>;
  loadByRun: (scope: ProductDataScope, runId: string) => Promise<void>;
  loadMoreEvents: () => Promise<void>;
  followConversation: (workspaceId: string, conversationId: string) => Promise<void>;
  focusedAddress: ViewAddress | null;
  /// The run the right rail is currently focused on (latest for the active
  /// conversation). Null when no complex task is in flight.
  activeRun: TaskRun | null;
  plan: PlanRevision | null;
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
  cancel: (runId: string | RunAddress) => Promise<TaskRunControlReceipt | undefined>;
  pause: (runId: string | RunAddress) => Promise<TaskRunControlReceipt | undefined>;
  updateGoal: (
    runId: string | RunAddress,
    expectedRevision: number,
    goal: string,
    reason: string
  ) => Promise<boolean>;
  updateContinuationBudgets: (
    runId: string | RunAddress,
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
  updateTasks: (
    reason: string,
    operations: TaskUpdateOperation[],
    targetRunId?: string | RunAddress
  ) => Promise<void>;
  insertTask: (
    afterTaskId: string | null,
    task: TaskSpec,
    targetRunId?: string | RunAddress
  ) => Promise<void>;
  skipTask: (taskId: string, targetRunId?: string | RunAddress) => Promise<void>;
  updateTask: (
    taskId: string,
    patch: Partial<TaskPatch>,
    targetRunId?: string | RunAddress
  ) => Promise<void>;
  reorderTasks: (newOrder: string[], targetRunId?: string | RunAddress) => Promise<void>;
  resumeTaskRun: (target?: RunAddress) => Promise<TaskRunResumeReceipt | undefined>;
  retryBlockedTask: (
    taskId: string,
    targetRunId?: string | RunAddress
  ) => Promise<TaskRetryReceipt | undefined>;
  resolveRecoveryTask: (
    taskId: string,
    decision: 'skip',
    targetRunId?: string | RunAddress
  ) => Promise<void>;
  skipGoalRequirement: (
    requirementId: string,
    reason: string,
    targetRunId?: string | RunAddress
  ) => Promise<void>;
  reset: () => void;
}

export const useTaskRuntimeStore = create<TaskRuntimeState>((set, get) => ({
  taskScope: null,
  historySnapshot: null,
  historyError: null,
  selectedRunId: null,
  runPage: [],
  runStatuses: [],
  nextRunContinuation: null,
  loadingRuns: false,
  runListError: null,
  loadingRun: false,
  hasMoreEvents: false,
  loadingEvents: false,
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
    if (!workspaceId || !runId || get().pollingInterval !== null) return;
    const active = (run: TaskRun | null | undefined) =>
      !!run && ['pending', 'running', 'paused'].includes(run.status);
    const interval = setInterval(() => {
      if (refreshInFlight || pollingTickInFlight) return;
      pollingTickInFlight = true;
      void (async () => {
        const chat = get().activeRun;
        if (active(chat) && chat) await get().refresh(chat.workspace_id, chat.run_id);
        const history = get().historySnapshot?.activeRun;
        if (active(history)) await get().refreshHistory();
        if (!active(get().activeRun) && !active(get().historySnapshot?.activeRun))
          get().stopPolling();
      })()
        .catch((error) => console.warn('[TaskRuntime] Poll observation failed:', error))
        .finally(() => {
          pollingTickInFlight = false;
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

  bindScope: (scope) => {
    const previous = get().taskScope;
    if (previous && sameProductDataScope(previous, scope)) return;
    if (
      (previous && !sameProductDataScope(previous, scope)) ||
      (get().activeRun && get().activeRun?.workspace_id !== scope.workspaceId)
    ) {
      get().reset();
    }
    historyRequestGeneration += 1;
    set({
      taskScope: { ...scope },
      historySnapshot: null,
      historyError: null,
      selectedRunId: null,
    });
  },

  loadRuns: async (scope, statuses = [], more = false) => {
    if (!get().taskScope || !sameProductDataScope(get().taskScope as ProductDataScope, scope))
      get().bindScope(scope);
    const filters = more ? get().runStatuses : statuses;
    const cursor = more ? get().nextRunContinuation : null;
    // 没有后续位置就是无操作，不能退役正在读取的第一页及其busy owner。
    if (more && !cursor) return;
    const request = ++inventoryRequestGeneration;
    set({ loadingRuns: true, runListError: null });
    try {
      const page = await taskRuntimeApi.listRuns(scope, filters, cursor);
      if (
        request !== inventoryRequestGeneration ||
        !get().taskScope ||
        !sameProductDataScope(get().taskScope as ProductDataScope, scope)
      )
        return;
      if (
        page.items.length > 20 ||
        page.items.some((run) => run.workspace_id !== scope.workspaceId) ||
        (cursor !== null && page.next_continuation === cursor)
      )
        throw new Error('运行列表返回了无效目标或分页位置');
      set({
        runPage: page.items,
        nextRunContinuation: page.next_continuation,
        runStatuses: filters,
      });
    } catch (error) {
      if (request === inventoryRequestGeneration)
        set({ runListError: error instanceof Error ? error.message : String(error) });
    } finally {
      if (request === inventoryRequestGeneration) set({ loadingRuns: false });
    }
  },

  loadByRun: async (scope, runId) => {
    if (!get().taskScope || !sameProductDataScope(get().taskScope as ProductDataScope, scope))
      get().bindScope(scope);
    const generation = ++historyRequestGeneration;
    eventRequestGeneration += 1;
    const previousSelection = get().selectedRunId;
    const previousView = get().historySnapshot;
    // 只有待载入同run的规范提交会退役旧快照；其它run刷新不吞掉本次选中的目标。
    const ownsInitialData = () => {
      const state = get();
      const current = state.historySnapshot;
      return (
        generation === historyRequestGeneration &&
        !!state.taskScope &&
        sameProductDataScope(state.taskScope, scope) &&
        (current?.activeRun?.run_id !== runId ||
          (current?.activeRun === previousView?.activeRun &&
            current?.plan === previousView?.plan &&
            current?.completionGate === previousView?.completionGate &&
            current?.events === previousView?.events))
      );
    };
    set({ selectedRunId: runId, loadingRun: true, loadingEvents: false, historyError: null });
    try {
      // Explicit history keeps its single snapshot even when this is also the live chat run.
      // A subsequent chat turn may select another run without changing this inspector's target.
      const run = await taskRuntimeApi.getRun(scope.workspaceId, runId, scope.workspaceGeneration);
      if (!ownsInitialData()) return;
      if (!run || run.run_id !== runId || run.workspace_id !== scope.workspaceId)
        throw new Error('所选运行已不可用或不属于此工作空间');
      const { snapshot, page } = await initialRunData(
        scope.workspaceId,
        run,
        scope.workspaceGeneration,
        false
      );
      if (!ownsInitialData() || hasNewerRunProjection(get(), run, snapshot.plan)) return;
      ingestTaskRuntimeSubagentEvents(run, snapshot.plan, page.events);
      ingestTaskRuntimeToolExecutions(run, page.events);
      set({
        historySnapshot: {
          ...snapshot,
          activeRun: run,
          events: page.events,
          lastSeq: page.next_after_sequence,
          hasMoreEvents: page.has_more,
          error: null,
        },
        historyError: null,
      });
      if (['pending', 'running', 'paused'].includes(run.status))
        get().startPolling(scope.workspaceId, runId);
    } catch (error) {
      if (ownsInitialData())
        set({
          selectedRunId: previousSelection,
          historyError: error instanceof Error ? error.message : String(error),
        });
    } finally {
      if (generation === historyRequestGeneration) set({ loadingRun: false });
    }
  },

  refreshHistory: async () => {
    const view = get().historySnapshot;
    const run = view?.activeRun;
    const scope = get().taskScope;
    if (!view || !run || !scope || get().selectedRunId !== run.run_id) return;
    const generation = historyRequestGeneration;
    const chatGeneration = loadGeneration;
    const requestGeneration = ++historyRefreshRequestGeneration;
    const after = view.lastSeq;
    refreshInFlight = true;
    try {
      const [updated, rawPage] = await Promise.all([
        taskRuntimeApi.getRun(scope.workspaceId, run.run_id, scope.workspaceGeneration),
        taskRuntimeApi.listEvents(scope.workspaceId, run.run_id, after, scope.workspaceGeneration),
      ]);
      if (
        generation !== historyRequestGeneration ||
        requestGeneration !== historyRefreshRequestGeneration ||
        get().historySnapshot?.activeRun?.run_id !== run.run_id ||
        get().historySnapshot?.lastSeq !== after
      )
        return;
      if (!updated || updated.run_id !== run.run_id || updated.workspace_id !== scope.workspaceId)
        throw new Error('历史运行已不可用或目标不匹配');
      const page = checkEventPage(rawPage, run.run_id, after);
      const snapshot = await loadRunSnapshot(scope.workspaceId, updated, scope.workspaceGeneration);
      if (
        generation !== historyRequestGeneration ||
        requestGeneration !== historyRefreshRequestGeneration ||
        get().historySnapshot?.activeRun?.run_id !== run.run_id ||
        get().historySnapshot?.lastSeq !== after
      )
        return;
      if (hasNewerRunProjection(get(), updated, snapshot.plan)) return;
      ingestTaskRuntimeSubagentEvents(updated, snapshot.plan, page.events);
      ingestTaskRuntimeToolExecutions(updated, page.events);
      set({
        historySnapshot: {
          ...snapshot,
          activeRun: updated,
          events: [...(get().historySnapshot?.events ?? []), ...page.events].slice(-MAX_EVENTS),
          lastSeq: page.next_after_sequence,
          hasMoreEvents: page.has_more,
          error: null,
        },
        loadingRun: false,
        ...(chatGeneration === loadGeneration &&
        get().activeRun?.run_id === updated.run_id &&
        get().activeRun?.workspace_id === updated.workspace_id
          ? { ...snapshot, activeRun: updated, error: null }
          : {}),
      });
      if (['pending', 'running', 'paused'].includes(updated.status))
        get().startPolling(updated.workspace_id, updated.run_id);
    } catch (error) {
      if (
        generation === historyRequestGeneration &&
        requestGeneration === historyRefreshRequestGeneration &&
        get().historySnapshot?.activeRun?.run_id === run.run_id
      )
        set({
          historySnapshot: {
            ...(get().historySnapshot as TaskRuntimeView),
            error: error instanceof Error ? error.message : String(error),
          },
        });
    } finally {
      if (requestGeneration === historyRefreshRequestGeneration) refreshInFlight = false;
    }
  },

  followConversation: async (workspaceId, conversationId) => {
    historyRequestGeneration += 1;
    historyRefreshRequestGeneration += 1;
    eventRequestGeneration += 1;
    refreshInFlight = false;
    set({
      selectedRunId: null,
      historySnapshot: null,
      historyError: null,
      loadingRun: false,
      loadingEvents: false,
    });
    await get().loadByConversation(workspaceId, conversationId);
  },

  refresh: async (workspaceId: string, runId: string) => {
    if (get().historySnapshot?.activeRun?.run_id === runId && get().activeRun?.run_id !== runId) {
      await get().refreshHistory();
      return;
    }
    const generation = loadGeneration;
    const historyGeneration = historyRequestGeneration;
    const requestGeneration = ++refreshRequestGeneration;
    const scope = get().taskScope;
    if (scope && scope.workspaceId !== workspaceId) return;
    const after = get().lastSeq;
    refreshInFlight = true;
    try {
      const [run, rawPage] = await Promise.all([
        taskRuntimeApi.getRun(workspaceId, runId, scope?.workspaceGeneration),
        taskRuntimeApi.listEvents(workspaceId, runId, after, scope?.workspaceGeneration),
      ]);
      if (generation !== loadGeneration || requestGeneration !== refreshRequestGeneration) return;
      if (!run || run.run_id !== runId || run.workspace_id !== workspaceId)
        throw new Error(`TaskRuntime run ${runId} 暂时不可用或目标不匹配`);
      const page = checkEventPage(rawPage, runId, after);
      const snapshot = await loadRunSnapshot(workspaceId, run, scope?.workspaceGeneration);
      if (
        generation !== loadGeneration ||
        requestGeneration !== refreshRequestGeneration ||
        get().lastSeq !== after
      )
        return;
      if (hasNewerRunProjection(get(), run, snapshot.plan)) return;
      ingestTaskRuntimeSubagentEvents(run, snapshot.plan, page.events);
      ingestTaskRuntimeToolExecutions(run, page.events);
      const current: TaskRuntimeView = {
        ...snapshot,
        activeRun: run,
        events: [...get().events, ...page.events].slice(-MAX_EVENTS),
        lastSeq: page.next_after_sequence,
        hasMoreEvents: page.has_more,
        error: null,
      };
      const history = get().historySnapshot;
      set({
        ...current,
        ...(historyGeneration === historyRequestGeneration && get().selectedRunId === runId
          ? {
              historySnapshot:
                history?.activeRun?.run_id === runId &&
                history.activeRun.workspace_id === workspaceId
                  ? { ...history, ...snapshot, activeRun: run, error: null }
                  : current,
              loadingRun: false,
            }
          : {}),
      });
      if (['pending', 'running', 'paused'].includes(run.status))
        get().startPolling(workspaceId, runId);
    } catch (error) {
      if (generation === loadGeneration && requestGeneration === refreshRequestGeneration)
        set({ error: error instanceof Error ? error.message : String(error) });
    } finally {
      if (requestGeneration === refreshRequestGeneration) refreshInFlight = false;
    }
  },

  loadMoreEvents: async () => {
    const view = taskRuntimePresentation(get());
    const run = view.activeRun;
    if (!run || get().loadingEvents || !view.hasMoreEvents) return;
    const generation = ++eventRequestGeneration;
    set({ loadingEvents: true });
    try {
      await get().refresh(run.workspace_id, run.run_id);
    } finally {
      if (generation === eventRequestGeneration) set({ loadingEvents: false });
    }
  },

  loadByConversation: async (workspaceId: string, conversationId: string) => {
    const generation = ++loadGeneration;
    if (!get().selectedRunId) {
      eventRequestGeneration += 1;
      set({ loadingEvents: false });
    }
    refreshRequestGeneration += 1;
    refreshInFlight = false;
    get().stopPolling();
    const previousView = get();
    const scope = previousView.taskScope;
    let targetRunId: string | undefined;
    // 确认目标后只受同run规范提交退役；旧run刷新不能吞掉同会话的新run载入。
    const ownsInitialData = () => {
      const current = get();
      return (
        generation === loadGeneration &&
        (scope
          ? !!current.taskScope && sameProductDataScope(current.taskScope, scope)
          : !current.taskScope) &&
        ((targetRunId !== undefined && current.activeRun?.run_id !== targetRunId) ||
          (current.activeRun === previousView.activeRun &&
            current.plan === previousView.plan &&
            current.completionGate === previousView.completionGate &&
            current.events === previousView.events))
      );
    };
    const token = scope?.workspaceId === workspaceId ? scope.workspaceGeneration : undefined;
    set({ focusedAddress: viewAddress(workspaceId, conversationId) });
    try {
      const run = await taskRuntimeApi.latestRunForConversation(workspaceId, conversationId, token);
      targetRunId = run?.run_id;
      if (!ownsInitialData()) return;
      if (run) {
        if (run.workspace_id !== workspaceId || run.conversation_id !== conversationId)
          throw new Error('TaskRuntime 返回了不属于当前会话的运行');
        const { snapshot, page, persistedTools } = await initialRunData(workspaceId, run, token);
        if (!ownsInitialData() || hasNewerRunProjection(get(), run, snapshot.plan)) return;
        ingestTaskRuntimeSubagentEvents(run, snapshot.plan, page.events);
        useToolExecutionStore
          .getState()
          .hydrateConversation(
            workspaceId,
            run.conversation_id,
            mergeTaskRuntimeToolExecutions(
              persistedTools,
              taskRuntimeToolExecutions(run, page.events)
            )
          );
        set({
          ...snapshot,
          activeRun: run,
          events: page.events,
          lastSeq: page.next_after_sequence,
          hasMoreEvents: page.has_more,
          error: null,
        });
        if (['pending', 'running', 'paused'].includes(run.status))
          get().startPolling(workspaceId, run.run_id);
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
          hasMoreEvents: false,
          error: null,
        });
      }
    } catch (error) {
      if (ownsInitialData()) set({ error: error instanceof Error ? error.message : String(error) });
    } finally {
      if (generation === loadGeneration) {
        const current = get().activeRun;
        const history = get().historySnapshot?.activeRun;
        const pending = [current, history].find(
          (run) => run && ['pending', 'running', 'paused'].includes(run.status)
        );
        if (pending) get().startPolling(pending.workspace_id, pending.run_id);
      }
    }
  },

  cancel: (runId) =>
    runAction(
      get,
      set,
      runId,
      (address) =>
        taskRuntimeApi.cancelRun(
          address.workspace_id,
          address.run_id,
          address.workspace_generation
        ),
      false,
      undefined,
      true
    ),

  pause: (runId) =>
    runAction(get, set, runId, (address) =>
      taskRuntimeApi.pauseRun(address.workspace_id, address.run_id, address.workspace_generation)
    ),

  updateGoal: async (runId, expectedRevision, goal, reason) => {
    const result = await runAction(
      get,
      set,
      runId,
      (address) =>
        taskRuntimeApi.updateGoal(
          address.workspace_id,
          address.run_id,
          expectedRevision,
          goal,
          reason,
          address.workspace_generation
        ),
      true,
      (updated, current) => {
        const previous = current.activeRun;
        if (
          !previous ||
          updated.run_id !== previous.run_id ||
          updated.workspace_id !== previous.workspace_id
        )
          throw new Error('目标更新回执不属于原工作空间或运行');
        const candidates = [get().activeRun, get().historySnapshot?.activeRun, updated].filter(
          (run): run is TaskRun =>
            !!run && run.run_id === updated.run_id && run.workspace_id === updated.workspace_id
        );
        const canonical = candidates.reduce((latest, run) =>
          run.goal_revision > latest.goal_revision ? run : latest
        );
        return {
          activeRun: canonical.goal_revision > previous.goal_revision ? canonical : previous,
        };
      },
      true,
      true
    );
    return result !== undefined;
  },

  updateContinuationBudgets: async (runId, tokenBudget, timeBudgetSeconds) => {
    await runAction(get, set, runId, (address) =>
      taskRuntimeApi.configureContinuation(
        address.workspace_id,
        address.run_id,
        tokenBudget,
        timeBudgetSeconds,
        address.workspace_generation
      )
    );
  },

  openInterruptPrompt: (data) => set({ interruptPrompt: data }),
  dismissInterruptPrompt: () => set({ interruptPrompt: null }),

  updateTasks: async (reason, operations, targetRunId) => {
    await runAction(
      get,
      set,
      targetRunId,
      async (address, view) => {
        if (view?.plan?.revision === undefined) throw new Error('当前任务图尚未就绪，无法修改');
        return taskRuntimeApi.updateTasks(
          address.workspace_id,
          address.run_id,
          {
            base_revision: view.plan.revision,
            reason,
            operations,
          },
          address.workspace_generation
        );
      },
      true,
      (plan) => ({ plan, error: null })
    );
  },
  insertTask: async (afterTaskId, task, targetRunId) => {
    await get().updateTasks(
      `新增任务：${task.title}`,
      [{ op: 'insert', after_task_id: afterTaskId, task }],
      targetRunId
    );
  },
  skipTask: async (taskId, targetRunId) => {
    await get().updateTasks(`跳过任务：${taskId}`, [{ op: 'skip', task_id: taskId }], targetRunId);
  },
  updateTask: async (taskId, patch, targetRunId) => {
    await get().updateTasks(
      `更新任务：${taskId}`,
      [{ op: 'update', task_id: taskId, patch: completeTaskPatch(patch) }],
      targetRunId
    );
  },
  reorderTasks: async (newOrder, targetRunId) => {
    await get().updateTasks('调整任务顺序', [{ op: 'reorder', task_ids: newOrder }], targetRunId);
  },
  resumeTaskRun: (target) => {
    const prompt = get().interruptPrompt;
    return runAction(get, set, target, async (address) => {
      const receipt = await taskRuntimeApi.resumeRun(
        address.workspace_id,
        address.run_id,
        address.workspace_generation
      );
      if (receipt.run_id !== address.run_id) throw new Error('恢复回执不属于原运行');
      if (
        prompt?.runId === address.run_id &&
        get().interruptPrompt === prompt &&
        get().taskScope?.workspaceGeneration === address.workspace_generation &&
        get().taskScope?.workspaceId === address.workspace_id
      )
        set({ interruptPrompt: null });
      return receipt;
    });
  },
  retryBlockedTask: (taskId, targetRunId) => {
    const prompt = get().interruptPrompt;
    return runAction(get, set, targetRunId, async (address) => {
      const receipt = await taskRuntimeApi.retryBlockedTask(
        address.workspace_id,
        address.run_id,
        taskId,
        address.workspace_generation
      );
      if (receipt.run_id !== address.run_id || receipt.task_id !== taskId)
        throw new Error('重试回执不属于原任务');
      if (
        prompt?.runId === address.run_id &&
        get().interruptPrompt === prompt &&
        get().taskScope?.workspaceGeneration === address.workspace_generation &&
        get().taskScope?.workspaceId === address.workspace_id
      )
        set({ interruptPrompt: null });
      return receipt;
    });
  },
  resolveRecoveryTask: async (taskId, decision, targetRunId) => {
    await runAction(get, set, targetRunId, (address) =>
      taskRuntimeApi.resolveRecoveryTask(
        address.workspace_id,
        address.run_id,
        taskId,
        decision,
        address.workspace_generation
      )
    );
  },
  skipGoalRequirement: async (requirementId, reason, targetRunId) => {
    await runAction(
      get,
      set,
      targetRunId,
      (address, view) => {
        if (!view?.activeRun) throw new Error('目标运行尚未载入');
        return taskRuntimeApi.skipGoalRequirement(
          address.workspace_id,
          address.run_id,
          view.activeRun.goal_revision,
          requirementId,
          reason,
          address.workspace_generation
        );
      },
      true,
      (completionGate) => ({ completionGate, error: null })
    );
  },

  reset: () => {
    loadGeneration += 1;
    inventoryRequestGeneration += 1;
    historyRequestGeneration += 1;
    eventRequestGeneration += 1;
    refreshRequestGeneration += 1;
    refreshInFlight = false;
    get().stopPolling();
    set({
      taskScope: null,
      historySnapshot: null,
      historyError: null,
      selectedRunId: null,
      runPage: [],
      runStatuses: [],
      nextRunContinuation: null,
      loadingRuns: false,
      runListError: null,
      loadingRun: false,
      hasMoreEvents: false,
      loadingEvents: false,
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
