//! TaskRuntime panels.
//!
//! Renders the structured state of a complex-task run from the canonical
//! file-backed TaskRuntime store (via taskRuntimeStore), NOT from regex-scanned chat messages.
//! Shows the run header, plan, live todo status, recovery actions, and artifacts.
//!
//! RightRail mounts the run summary and plan; task details reuse the shared Modal.

import { useEffect, useMemo, useRef, useState } from 'react';
import {
  CheckCircle2,
  Circle,
  Loader2,
  AlertCircle,
  ListTodo,
  RefreshCw,
  Gauge,
  Trash2,
  Play,
  Pause,
  RotateCcw,
  SkipForward,
  Edit3,
  X,
  AlertTriangle,
  Clock3,
  Save,
} from 'lucide-react';
import { Card } from '../common/Card';
import {
  TaskRuntimeList,
  taskTimestamp,
  type TaskAction,
  type TaskActionReceipt,
} from './TaskRuntimeList';
import './TaskRuntimePanel.css';
import { useTaskRuntimeStore, taskRuntimePresentation } from '../../stores/taskRuntimeStore';
import {
  useSubagentRunStore,
  latestSubagentRunsByTask,
  type ExecutionEvent,
  type SubagentRunState,
} from '../../stores/subagentRunStore';
import type {
  RunContinuationState,
  RunPauseReason,
  TaskRunStatus,
  TodoStatus,
} from '../../generated';
import { isCanonicalUsageEvent } from '../compress/subagentUsage';

const STATUS_LABEL: Record<string, string> = {
  pending: '待处理',
  running: '执行中',
  paused: '已暂停',
  cancelled: '已取消',
  failed: '失败',
  completed: '已完成',
};

const TODO_LABEL: Record<string, string> = {
  pending: '待处理',
  running: '执行中',
  blocked: '阻塞',
  completed: '已完成',
  failed: '失败',
  cancelled: '已取消',
  timed_out: '已超时',
  skipped: '已跳过',
};

const REQUIREMENT_LABEL: Record<string, string> = {
  pending: '待验收',
  accepted: '已验收',
  skipped: '已确认跳过',
  stale: '证据已失效',
  failed: '验收失败',
};

const PAUSE_REASON_LABEL: Record<RunPauseReason, string> = {
  user: '用户暂停',
  needs_input: '等待补充信息',
  approval: '等待确认',
  boot_recovery: '启动恢复',
  usage_limit: '用量限制',
  token_budget: 'Token 预算已耗尽',
  time_budget: '时间预算已耗尽',
  repeated_blocker: '连续受阻',
  indeterminate_side_effect: '副作用状态待确认',
  provider_unavailable: '模型服务暂不可用',
};

const SUBAGENT_STATUS_LABEL: Record<SubagentRunState['status'], string> = {
  running: '执行中',
  completed: '执行已完成',
  failed: '执行失败',
  cancelled: '执行已取消',
  timed_out: '执行超时',
};

type ContinuationBudget = Pick<
  RunContinuationState,
  'token_budget' | 'time_budget_seconds' | 'tokens_used' | 'time_used_seconds'
>;

function normalizedAmount(value: number): number {
  return Number.isFinite(value) ? Math.max(0, Math.floor(value)) : 0;
}

export function parseContinuationBudgetInput(value: string, label: string): number | null {
  const trimmed = value.trim();
  if (!trimmed) return null;
  const parsed = Number(trimmed);
  if (!Number.isSafeInteger(parsed) || parsed <= 0) {
    throw new Error(`${label}必须是正整数，留空表示不限`);
  }
  return parsed;
}

export function formatDurationSeconds(value: number): string {
  let remaining = normalizedAmount(value);
  const days = Math.floor(remaining / 86_400);
  remaining %= 86_400;
  const hours = Math.floor(remaining / 3_600);
  remaining %= 3_600;
  const minutes = Math.floor(remaining / 60);
  const seconds = remaining % 60;
  const parts: string[] = [];
  if (days > 0) parts.push(`${days} 天`);
  if (hours > 0) parts.push(`${hours} 小时`);
  if (minutes > 0) parts.push(`${minutes} 分钟`);
  if (seconds > 0 || parts.length === 0) parts.push(`${seconds} 秒`);
  return parts.join(' ');
}

function formatTokenCount(value: number): string {
  return normalizedAmount(value).toLocaleString('en-US');
}

function formatBudgetLine(
  label: string,
  used: number,
  budget: number | null | undefined,
  format: (value: number) => string
): string {
  const normalizedUsed = normalizedAmount(used);
  if (budget === null || budget === undefined) {
    return `${label}已用 ${format(normalizedUsed)} · 预算不限 · 剩余不限`;
  }
  const normalizedBudget = normalizedAmount(budget);
  const remaining = Math.max(0, normalizedBudget - normalizedUsed);
  return `${label}已用 ${format(normalizedUsed)} · 预算 ${format(normalizedBudget)} · 剩余 ${format(remaining)}`;
}

export function continuationBudgetLabels(continuation: ContinuationBudget): {
  tokens: string;
  time: string;
} {
  return {
    tokens: formatBudgetLine(
      'Token ',
      continuation.tokens_used,
      continuation.token_budget,
      formatTokenCount
    ),
    time: formatBudgetLine(
      '时间',
      continuation.time_used_seconds,
      continuation.time_budget_seconds,
      formatDurationSeconds
    ),
  };
}

function statusColor(status: string): string {
  if (['completed'].includes(status)) return 'var(--color-success)';
  if (['running'].includes(status)) return 'var(--color-info)';
  if (['failed', 'cancelled', 'timed_out'].includes(status)) return 'var(--color-error)';
  if (['paused', 'blocked'].includes(status)) return 'var(--color-warning)';
  return 'var(--text-tertiary)';
}

function TodoIcon({
  status,
  executionStatus,
  taskRunStatus,
}: {
  status: string;
  executionStatus?: SubagentRunState['status'];
  taskRunStatus: TaskRunStatus;
}) {
  if (!todoShouldSpin(status, executionStatus, taskRunStatus) && status === 'running') {
    if (executionStatus === 'failed' || executionStatus === 'timed_out') {
      return <AlertCircle size={14} style={{ color: 'var(--color-error)' }} />;
    }
    return <Clock3 size={14} style={{ color: 'var(--text-tertiary)' }} />;
  }
  switch (status) {
    case 'completed':
      return <CheckCircle2 size={14} style={{ color: 'var(--color-success)' }} />;
    case 'running':
      return <Loader2 size={14} className="animate-spin" style={{ color: 'var(--color-info)' }} />;
    case 'failed':
    case 'timed_out':
      return <AlertCircle size={14} style={{ color: 'var(--color-error)' }} />;
    case 'cancelled':
      return <Circle size={14} style={{ color: 'var(--color-error)' }} />;
    case 'blocked':
      return <AlertCircle size={14} style={{ color: 'var(--color-warning)' }} />;
    case 'skipped':
      return <Circle size={14} style={{ color: 'var(--text-tertiary)' }} />;
    default:
      return <Circle size={14} style={{ color: 'var(--text-tertiary)' }} />;
  }
}

export function todoShouldSpin(
  status: string,
  executionStatus: SubagentRunState['status'] | undefined,
  taskRunStatus: TaskRunStatus
): boolean {
  return (
    status === 'running' &&
    taskRunStatus === 'running' &&
    (!executionStatus || executionStatus === 'running')
  );
}

function uniqueValues(values: Array<string | null | undefined>): string[] {
  return [...new Set(values.filter((value): value is string => Boolean(value && value.trim())))];
}

function num(value: number | undefined): number {
  return typeof value === 'number' && Number.isFinite(value) ? value : 0;
}

interface CacheUsageSummary {
  calls: number;
  missingUsage: number;
  inputTokens: number;
  outputTokens: number;
  totalTokens: number;
  cachedPromptTokens: number;
  cacheCreationPromptTokens: number;
  models: string[];
}

interface CacheDiagnostic {
  severity: 'good' | 'warn' | 'info';
  label: string;
  detail: string;
}

// Cache diagnostics now read from `usage` events emitted by the framework's
// DispatchLlmUsage (Phase 3a). These carry the full breakdown on the event
// top level — no more payload digging, and no thinking_end fallback needed
// (LlmUsage covers every model call).
function cacheUsageFromEvents(events: ExecutionEvent[]): CacheUsageSummary {
  const seenEventIds = new Set<string>();
  const usageEvents = events.filter((event) => {
    if (!isCanonicalUsageEvent(event)) return false;
    if (!event.usage_event_id) return true;
    if (seenEventIds.has(event.usage_event_id)) return false;
    seenEventIds.add(event.usage_event_id);
    return true;
  });
  const models = uniqueValues(usageEvents.map((e) => e.model));
  return usageEvents.reduce<CacheUsageSummary>(
    (summary, event) => {
      summary.calls += 1;
      if (event.usage_reported === false) {
        summary.missingUsage += 1;
      } else {
        const inputTokens = num(event.prompt_tokens);
        const outputTokens = num(event.completion_tokens);
        const totalTokens = num(event.total_tokens) || inputTokens + outputTokens;
        summary.inputTokens += inputTokens;
        summary.outputTokens += outputTokens;
        summary.totalTokens += totalTokens;
        summary.cachedPromptTokens += num(event.cached_prompt_tokens);
        summary.cacheCreationPromptTokens += num(event.cache_creation_prompt_tokens);
      }
      return summary;
    },
    {
      calls: 0,
      missingUsage: 0,
      inputTokens: 0,
      outputTokens: 0,
      totalTokens: 0,
      cachedPromptTokens: 0,
      cacheCreationPromptTokens: 0,
      models,
    }
  );
}

export function cacheUsageForRuns(runs: SubagentRunState[]): CacheUsageSummary {
  return cacheUsageFromEvents(runs.flatMap((run) => run.usageEvents ?? []));
}

function cacheReadRate(summary: CacheUsageSummary): number | null {
  if (summary.inputTokens <= 0) return null;
  return summary.cachedPromptTokens / summary.inputTokens;
}

function cacheDiagnostics(summary: CacheUsageSummary): CacheDiagnostic[] {
  if (summary.calls === 0) return [];
  const diagnostics: CacheDiagnostic[] = [];
  const rate = cacheReadRate(summary);

  if (summary.missingUsage > 0) {
    diagnostics.push({
      severity: 'warn',
      label: 'provider usage 缺失',
      detail: `${summary.missingUsage} 次 LLM 请求没有返回 usage 元数据，缓存命中率会被低估或无法判断。`,
    });
  }

  if (summary.models.length > 1) {
    diagnostics.push({
      severity: 'warn',
      label: '模型不一致',
      detail: `本次任务使用了 ${summary.models.length} 个模型。不同模型通常不会共享 provider prompt cache。`,
    });
  }

  if (
    rate != null &&
    summary.inputTokens > 0 &&
    summary.cachedPromptTokens === 0 &&
    summary.missingUsage < summary.calls
  ) {
    diagnostics.push({
      severity: 'warn',
      label: '没有 cache read',
      detail:
        'provider 已返回 usage，但 cached prompt tokens 为 0。优先检查 system prefix、tools 定义、subagent prompt 和动态上下文是否每轮变化。',
    });
  } else if (rate != null && rate < 0.2 && summary.inputTokens >= 1000) {
    diagnostics.push({
      severity: 'warn',
      label: 'cache read 偏低',
      detail: `当前 read rate ${(rate * 100).toFixed(1)}%。建议对比同模型同任务下的 system prefix、tools 顺序、cwd/记忆/hook 注入位置。`,
    });
  } else if (rate != null && rate >= 0.8) {
    diagnostics.push({
      severity: 'good',
      label: 'cache read 良好',
      detail: `当前 read rate ${(rate * 100).toFixed(1)}%，说明稳定前缀大概率已被 provider 复用。`,
    });
  }

  if (
    summary.cacheCreationPromptTokens > summary.cachedPromptTokens &&
    summary.cacheCreationPromptTokens > 0
  ) {
    diagnostics.push({
      severity: 'info',
      label: 'cache write 高于 read',
      detail:
        '本轮更多是在创建缓存而非读取缓存。连续重复同类任务时，如果 read 仍不升高，再检查前缀稳定性。',
    });
  }

  if (diagnostics.length === 0 && summary.calls > 0) {
    diagnostics.push({
      severity: 'info',
      label: '数据不足',
      detail:
        '当前 usage 数据不足以判断命中低的原因。重复同模型同提示词任务后再观察 cached tokens 和 read rate。',
    });
  }

  return diagnostics;
}

function diagnosticColor(severity: CacheDiagnostic['severity']): string {
  if (severity === 'good') return 'var(--color-success)';
  if (severity === 'warn') return 'var(--color-warning)';
  return 'var(--text-tertiary)';
}

export function CacheUsageCard({
  summary,
  compact = false,
}: {
  summary: CacheUsageSummary;
  compact?: boolean;
}) {
  if (summary.calls === 0) return null;
  const rate = cacheReadRate(summary);
  const diagnostics = cacheDiagnostics(summary);
  const valueClass = compact ? 'text-[11px]' : 'text-sm';
  const labelClass = compact ? 'text-[9px]' : 'text-[10px]';

  return (
    <Card variant="elevated" className={`bg-[var(--bg-secondary)] ${compact ? 'p-2' : 'p-3'}`}>
      <div className="mb-2 flex items-center justify-between gap-2">
        <div className="flex min-w-0 items-center gap-1.5">
          <Gauge size={compact ? 12 : 14} style={{ color: 'var(--accent)' }} />
          <span
            className={compact ? 'text-[10px] font-medium' : 'text-[12px] font-medium'}
            style={{ color: 'var(--text-primary)' }}
          >
            Token / Cache
          </span>
        </div>
        <span className="shrink-0 text-[9px]" style={{ color: 'var(--text-tertiary)' }}>
          {summary.calls} LLM call{summary.calls > 1 ? 's' : ''}
          {summary.missingUsage > 0 && (
            <span style={{ color: 'var(--color-warning)' }}> ({summary.missingUsage} 未上报)</span>
          )}
        </span>
      </div>
      <div className="grid grid-cols-3 gap-1.5">
        <MetricCell
          label="Input"
          value={summary.inputTokens.toLocaleString()}
          valueClass={valueClass}
          labelClass={labelClass}
        />
        <MetricCell
          label="Output"
          value={summary.outputTokens.toLocaleString()}
          valueClass={valueClass}
          labelClass={labelClass}
        />
        <MetricCell
          label="Cached"
          value={summary.cachedPromptTokens.toLocaleString()}
          valueClass={valueClass}
          labelClass={labelClass}
        />
        <MetricCell
          label="Cache write"
          value={summary.cacheCreationPromptTokens.toLocaleString()}
          valueClass={valueClass}
          labelClass={labelClass}
        />
        <MetricCell
          label="Read rate"
          value={rate == null ? 'unknown' : `${(rate * 100).toFixed(1)}%`}
          valueClass={valueClass}
          labelClass={labelClass}
        />
        <MetricCell
          label="Missing usage"
          value={summary.missingUsage.toLocaleString()}
          valueClass={valueClass}
          labelClass={labelClass}
        />
      </div>
      {summary.models.length > 0 && (
        <div
          className="mt-2 truncate text-[9px]"
          style={{ color: 'var(--text-tertiary)' }}
          title={summary.models.join(', ')}
        >
          model: {summary.models.join(', ')}
        </div>
      )}
      {summary.missingUsage > 0 && (
        <div className="mt-1 text-[9px] leading-snug" style={{ color: 'var(--color-warning)' }}>
          有 {summary.missingUsage} 次请求没有 provider usage 元数据；这些请求不会被计入缓存命中率。
        </div>
      )}
      {!compact && diagnostics.length > 0 && (
        <div className="mt-3 space-y-1.5">
          <div
            className="flex items-center gap-1 text-[10px] font-medium"
            style={{ color: 'var(--text-secondary)' }}
          >
            <AlertTriangle size={11} />
            缓存诊断
          </div>
          {diagnostics.map((diagnostic) => (
            <div
              key={`${diagnostic.label}:${diagnostic.detail}`}
              className="rounded-md px-2 py-1.5 text-[10px] leading-snug"
              style={{ background: 'var(--bg-hover)', color: 'var(--text-secondary)' }}
            >
              <div
                className="mb-0.5 font-medium"
                style={{ color: diagnosticColor(diagnostic.severity) }}
              >
                {diagnostic.label}
              </div>
              <div>{diagnostic.detail}</div>
            </div>
          ))}
        </div>
      )}
    </Card>
  );
}

function MetricCell({
  label,
  value,
  valueClass,
  labelClass,
}: {
  label: string;
  value: string;
  valueClass: string;
  labelClass: string;
}) {
  return (
    <div className="rounded-md px-2 py-1" style={{ background: 'var(--bg-primary)' }}>
      <div className={`${labelClass} truncate`} style={{ color: 'var(--text-tertiary)' }}>
        {label}
      </div>
      <div className={`${valueClass} truncate font-mono`} style={{ color: 'var(--text-primary)' }}>
        {value}
      </div>
    </div>
  );
}

function traceRunForTodo(todo: { task_id: string }, runs: SubagentRunState[]) {
  return latestSubagentRunsByTask(runs).find((run) => run.taskId === todo.task_id);
}

export function traceRunsForTaskRun(
  runId: string | null,
  runs: readonly SubagentRunState[]
): SubagentRunState[] {
  return runId
    ? runs.filter((run) => run.runId === runId).sort((a, b) => a.startedAt - b.startedAt)
    : [];
}

export function todoStatusDescription(
  todo: { status: TodoStatus; task_id: string },
  runs: SubagentRunState[]
): string {
  const status = todo.status;
  const execution = traceRunForTodo(todo, runs);
  if (!execution) return TODO_LABEL[status] ?? status;
  const executionLabel = SUBAGENT_STATUS_LABEL[execution.status];
  if (execution.status === 'completed' && status === 'blocked') {
    return `${executionLabel} · 评审未通过`;
  }
  if (execution.status === 'completed' && status === 'skipped') {
    return `${executionLabel} · 任务已跳过`;
  }
  if (execution.status === 'completed' && status === 'failed') {
    return `${executionLabel} · 任务失败`;
  }
  if (execution.status === 'completed' && status === 'completed') {
    return '执行与任务提交已完成';
  }
  if (execution.status === 'completed' && status === 'running') {
    return `${executionLabel} · 评审/收尾中`;
  }
  if (execution.status === 'running' && status === 'running') {
    return executionLabel;
  }
  return `${executionLabel} · 任务${TODO_LABEL[status] ?? status}`;
}

export function TaskRuntimePanel() {
  const traceRuns = useSubagentRunStore((s) => s.runs);
  const state = useTaskRuntimeStore();
  const {
    activeRun,
    plan,
    todos,
    artifacts,
    recoveryBlockers,
    continuation,
    backgroundCells,
    completionGate,
    error,
    hasMoreEvents,
  } = useTaskRuntimeStore(taskRuntimePresentation);
  const {
    selectedRunId,
    loadingEvents,
    loadMoreEvents,
    refresh,
    cancel,
    pause,
    updateGoal,
    updateContinuationBudgets,
    resumeTaskRun,
    retryBlockedTask,
    resolveRecoveryTask,
    skipGoalRequirement,
  } = state;
  const [tokenBudgetInput, setTokenBudgetInput] = useState('');
  const [timeBudgetInput, setTimeBudgetInput] = useState('');
  const [budgetError, setBudgetError] = useState<string | null>(null);
  const [pendingAction, setPendingAction] = useState<string | null>(null);
  const actionInFlight = useRef<symbol | null>(null);
  const [actionReceipt, setActionReceipt] = useState<TaskActionReceipt | null>(null);
  const [goalInput, setGoalInput] = useState('');
  const [goalReasonInput, setGoalReasonInput] = useState('');
  const [goalError, setGoalError] = useState<string | null>(null);
  const [requirementSkipId, setRequirementSkipId] = useState<string | null>(null);
  const [requirementSkipReason, setRequirementSkipReason] = useState('');

  useEffect(() => {
    actionInFlight.current = null;
    setPendingAction(null);
    setActionReceipt(null);
  }, [activeRun?.run_id, state.taskScope?.workspaceId, state.taskScope?.workspaceGeneration]);

  useEffect(() => {
    setTokenBudgetInput(continuation?.token_budget?.toString() ?? '');
    setTimeBudgetInput(continuation?.time_budget_seconds?.toString() ?? '');
    setBudgetError(null);
  }, [activeRun?.run_id, continuation?.token_budget, continuation?.time_budget_seconds]);

  useEffect(() => {
    setGoalInput(activeRun?.goal ?? '');
    setGoalReasonInput('');
    setGoalError(null);
    setRequirementSkipId(null);
    setRequirementSkipReason('');
  }, [activeRun?.run_id, activeRun?.goal_revision, activeRun?.goal]);

  const visibleTraceRuns = useMemo(
    () =>
      activeRun
        ? Object.values(traceRuns)
            // P1.0: each inline subagent now has its own run_id (independent of
            // the chat turn's root_message_id). Show ALL subagent runs that belong
            // to this conversation, not just the single activeRun. Falls back to
            // exact run_id match for non-inline (DAG) runs that don't carry a
            // conversationId.
            .filter(
              (run) =>
                run.runId === activeRun.run_id ||
                (!selectedRunId && run.conversationId === activeRun.conversation_id)
            )
            .sort((a, b) => a.startedAt - b.startedAt)
        : [],
    [activeRun, traceRuns, selectedRunId]
  );
  const activeTaskTraceRuns = useMemo(
    // A partial prefix can contain an old completed attempt followed by an unread retry.
    // Keep canonical Todo facts visible without presenting that prefix as the latest execution.
    () =>
      hasMoreEvents ? [] : traceRunsForTaskRun(activeRun?.run_id ?? null, Object.values(traceRuns)),
    [activeRun, traceRuns, hasMoreEvents]
  );

  if (!activeRun) return null;

  // Detailed execution timeline is now in ConversationTimeline (main panel).
  // Keep the plan summary and task details on the original runtime projection.
  const runId = activeRun.run_id;
  // The rendered target owns this token; a delayed click must not borrow a replacement host.
  const controlTarget = {
    workspace_id: activeRun.workspace_id,
    run_id: runId,
    workspace_generation:
      state.taskScope?.workspaceId === activeRun.workspace_id
        ? state.taskScope.workspaceGeneration
        : '',
  };
  const usageSummary = cacheUsageForRuns(visibleTraceRuns);
  const planGoalCurrent =
    plan?.goal_revision === activeRun.goal_revision && plan?.goal_sha256 === activeRun.goal_sha256;
  // 独立IPC读取可能跨越Goal/计划提交；只有同run和同revision报告可标为当前验收。
  const completionGateCurrent =
    completionGate?.run_id === activeRun.run_id &&
    completionGate.goal_revision === activeRun.goal_revision &&
    completionGate.plan_revision === (plan?.revision ?? 0) &&
    (!completionGate.ready || planGoalCurrent);
  const completedCount = planGoalCurrent
    ? todos.filter((todo) => todo.status === ('completed' as TodoStatus)).length
    : 0;
  const executionCompletedCount = planGoalCurrent
    ? todos.filter((todo) => {
        const trace = traceRunForTodo(todo, activeTaskTraceRuns);
        return trace?.status === 'completed' || todo.status === ('completed' as TodoStatus);
      }).length
    : 0;
  const currentTurn = continuation?.active_turn ?? continuation?.last_turn;
  const activeCellCount = backgroundCells.filter((cell) =>
    ['prepared', 'queued', 'running'].includes(cell.phase)
  ).length;
  const budgetLabels = continuation?.enabled ? continuationBudgetLabels(continuation) : null;
  const visibleActionReceipt =
    actionReceipt?.runId === runId &&
    actionReceipt.workspaceId === controlTarget.workspace_id &&
    actionReceipt.workspaceGeneration === controlTarget.workspace_generation
      ? actionReceipt
      : null;
  const performAction: TaskAction = async (label, operation, taskId) => {
    const ownsTarget = () => {
      const current = useTaskRuntimeStore.getState();
      const run = taskRuntimePresentation(current).activeRun;
      return (
        !!controlTarget.workspace_generation &&
        current.taskScope?.workspaceId === controlTarget.workspace_id &&
        current.taskScope.workspaceGeneration === controlTarget.workspace_generation &&
        run?.run_id === runId &&
        run.workspace_id === controlTarget.workspace_id
      );
    };
    if (actionInFlight.current || !ownsTarget()) return false;
    const operationToken = Symbol(label);
    actionInFlight.current = operationToken;
    // Capture the original target before awaiting; the selected detail may change meanwhile.
    const receiptIdentity = {
      runId,
      taskId,
      workspaceId: controlTarget.workspace_id,
      workspaceGeneration: controlTarget.workspace_generation,
    };
    setPendingAction(label);
    setActionReceipt(null);
    // A→B→A仍是新的访问；同身份旧Promise不能替代新请求的回执。
    const ownsAction = () => actionInFlight.current === operationToken && ownsTarget();
    try {
      const receipt = await operation();
      const snapshot = taskRuntimePresentation(useTaskRuntimeStore.getState());
      if (!ownsAction() || !snapshot.activeRun) return false;
      if (snapshot.error) {
        setActionReceipt({
          ...receiptIdentity,
          text: `${label}未确认：${snapshot.error}`,
          failed: true,
        });
        return false;
      }
      let outcome = '请求已返回';
      if (receipt && typeof receipt === 'object') {
        if ('run_id' in receipt && receipt.run_id !== runId)
          throw new Error('后端回执不属于原运行');
        if (taskId && 'task_id' in receipt && receipt.task_id !== taskId)
          throw new Error('后端回执不属于原任务');
        if ('success' in receipt && receipt.success === false)
          outcome = '无需重复执行（后端未改变运行）';
        else if ('success' in receipt && receipt.success === true)
          outcome = '后端已接受请求，结算以运行记录为准';
        else if ('kind' in receipt) {
          outcome =
            receipt.kind === 'retry_scheduled'
              ? '重试已排期'
              : receipt.kind === 'recovery_retry_recorded'
                ? '恢复重试已记录'
                : receipt.kind === 'continuation_resumed'
                  ? '继续请求已接受'
                  : receipt.kind === 'resumed'
                    ? '恢复请求已接受'
                    : '后端回执已返回';
        }
      }
      setActionReceipt({
        ...receiptIdentity,
        text: `${label}${outcome} · 当前运行${STATUS_LABEL[snapshot.activeRun.status] ?? snapshot.activeRun.status}`,
        failed: false,
      });
      return true;
    } catch (actionError) {
      if (!ownsAction()) return false;
      setActionReceipt({
        ...receiptIdentity,
        text: `${label}失败：${actionError instanceof Error ? actionError.message : String(actionError)}`,
        failed: true,
      });
      return false;
    } finally {
      if (actionInFlight.current === operationToken) {
        actionInFlight.current = null;
        setPendingAction(null);
      }
    }
  };

  const applyBudgets = async () => {
    try {
      const tokenBudget = parseContinuationBudgetInput(tokenBudgetInput, 'Token 预算');
      const timeBudget = parseContinuationBudgetInput(timeBudgetInput, '时间预算');
      setBudgetError(null);
      await performAction('更新预算', () =>
        updateContinuationBudgets(controlTarget, tokenBudget, timeBudget)
      );
    } catch (budgetInputError) {
      setBudgetError(
        budgetInputError instanceof Error ? budgetInputError.message : String(budgetInputError)
      );
    }
  };
  const applyGoal = async () => {
    const goal = goalInput.trim();
    const reason = goalReasonInput.trim();
    if (!goal || !reason) {
      setGoalError('目标和修改原因不能为空');
      return;
    }
    setGoalError(null);
    await performAction('更新目标', () =>
      updateGoal(controlTarget, activeRun.goal_revision, goal, reason)
    );
  };
  const confirmRequirementSkip = async () => {
    const reason = requirementSkipReason.trim();
    const requirementId = requirementSkipId;
    if (!requirementId || !reason) return;
    const succeeded = await performAction('确认跳过验收项', () =>
      skipGoalRequirement(requirementId, reason, controlTarget)
    );
    if (!succeeded) return;
    setRequirementSkipId(null);
    setRequirementSkipReason('');
  };

  return (
    <section className="eko-task-runtime">
      {hasMoreEvents && (
        <div className="eko-task-history-notice" role="status">
          <span>执行记录尚有后续页，当前执行明细可能不完整。</span>
          <button
            type="button"
            className="eko-task-button"
            disabled={loadingEvents}
            onClick={() => void loadMoreEvents()}
          >
            {loadingEvents ? '正在读取…' : '继续读取执行记录'}
          </button>
        </div>
      )}
      <div className="eko-task-run-summary">
        <div className="eko-task-run-eyebrow">
          <span>
            <ListTodo size={14} /> 任务运行
          </span>
          <span className="eko-task-run-status" style={{ color: statusColor(activeRun.status) }}>
            {STATUS_LABEL[activeRun.status] ?? activeRun.status}
          </span>
        </div>
        <h2>{activeRun.goal}</h2>
        <div className="eko-task-run-meta">
          <span>{activeRun.attended_mode === 'unattended' ? '后台触发' : '交互任务'}</span>
          <span>更新于 {taskTimestamp(activeRun.updated_at)}</span>
        </div>
        {planGoalCurrent ? (
          <>
            <div className="eko-task-progress-heading">
              <span>
                已提交完成 <strong>{completedCount}</strong>
                <span> / {todos.length} 项</span>
              </span>
              <span>
                {hasMoreEvents ? '执行记录尚未读完' : `执行完成 ${executionCompletedCount} 项`}
              </span>
            </div>
            {todos.length > 0 ? (
              <div
                className="eko-task-progress-track"
                aria-label={`已提交完成 ${completedCount} / ${todos.length} 项任务`}
              >
                <span style={{ width: `${(completedCount / todos.length) * 100}%` }} />
              </div>
            ) : (
              <p className="eko-task-progress-unknown">任务尚未展开 · 进度待记录</p>
            )}
          </>
        ) : plan ? (
          <p role="status" className="eko-task-progress-unknown">
            旧计划未绑定当前 Goal r{activeRun.goal_revision}，保留为历史记录，不计当前目标完成进度。
          </p>
        ) : (
          <p className="eko-task-progress-unknown">任务尚未展开 · 进度待记录</p>
        )}
        {continuation?.pause && (
          <div className="eko-task-wait-reason">
            <Clock3 size={14} />
            <span>
              {PAUSE_REASON_LABEL[continuation.pause.reason] ?? continuation.pause.reason}
              {continuation.pause.detail ? ` · ${continuation.pause.detail}` : ''}
            </span>
          </div>
        )}
      </div>
      {error && !visibleActionReceipt?.failed && (
        <div role="alert" className="eko-task-action-receipt is-error">
          {error}
        </div>
      )}
      {visibleActionReceipt && (
        <div
          role={visibleActionReceipt.failed ? 'alert' : 'status'}
          className={`eko-task-action-receipt ${visibleActionReceipt.failed ? 'is-error' : ''}`}
        >
          {visibleActionReceipt.text}
        </div>
      )}
      {pendingAction && (
        <div role="status" className="eko-task-action-receipt">
          <Loader2 size={14} className="animate-spin" />
          {pendingAction}请求处理中，等待运行记录返回…
        </div>
      )}
      {activeRun.status === 'running' && (
        <div className="mb-2 grid grid-cols-2 gap-1.5">
          <button
            onClick={() => void performAction('暂停', () => pause(controlTarget))}
            disabled={!!pendingAction}
            className="flex items-center justify-center gap-1 rounded-md px-2 py-1.5 text-[11px] font-medium"
            style={{ background: 'var(--bg-hover)', color: 'var(--text-primary)' }}
            title="暂停任务运行"
          >
            <Pause size={12} /> 暂停
          </button>
          <button
            onClick={() => void performAction('取消', () => cancel(controlTarget))}
            disabled={!!pendingAction}
            className="flex items-center justify-center gap-1 rounded-md px-2 py-1.5 text-[11px] font-medium"
            style={{ background: 'var(--bg-hover)', color: 'var(--color-error)' }}
            title="取消任务运行"
          >
            <Trash2 size={12} /> 取消
          </button>
        </div>
      )}

      {activeRun.status === 'paused' && recoveryBlockers.length === 0 && (
        <div className="mb-2">
          <button
            onClick={() => void performAction('继续执行', () => resumeTaskRun(controlTarget))}
            disabled={!planGoalCurrent || !!pendingAction}
            className="flex w-full items-center justify-center gap-1 rounded-md px-3 py-1.5 text-[11px] font-medium"
            style={{
              background: planGoalCurrent ? 'var(--accent)' : 'var(--bg-hover)',
              color: planGoalCurrent ? 'var(--text-on-accent)' : 'var(--text-tertiary)',
            }}
            title={planGoalCurrent ? '继续执行' : '任务图尚未绑定当前目标'}
          >
            <Play size={12} /> 继续执行
          </button>
        </div>
      )}

      <TaskRuntimeList
        controlTarget={controlTarget}
        key={runId}
        run={activeRun}
        plan={plan}
        todos={todos}
        traceRuns={activeTaskTraceRuns}
        artifacts={artifacts}
        recoveryBlockers={recoveryBlockers}
        pendingAction={pendingAction}
        actionReceipt={visibleActionReceipt}
        onAction={performAction}
        describeTodo={(todo) =>
          planGoalCurrent
            ? todoStatusDescription(todo, activeTaskTraceRuns)
            : `历史计划 · ${TODO_LABEL[todo.status] ?? todo.status}（不计当前目标）`
        }
        renderStatus={(todo) =>
          planGoalCurrent ? (
            <TodoIcon
              status={todo.status}
              executionStatus={traceRunForTodo(todo, activeTaskTraceRuns)?.status}
              taskRunStatus={activeRun.status}
            />
          ) : (
            <Clock3 size={14} aria-label="历史计划状态" style={{ color: 'var(--text-tertiary)' }} />
          )
        }
      />
      <details className="eko-task-runtime-settings">
        <summary>
          运行控制与验收 <span>Goal r{activeRun.goal_revision}</span>
        </summary>
        <div className="eko-task-runtime-settings-body">
          {activeRun.status === 'paused' && (
            <form
              className="mb-2 space-y-1"
              onSubmit={(event) => {
                event.preventDefault();
                void applyGoal();
              }}
            >
              <label className="block">
                <span className="sr-only">任务目标</span>
                <textarea
                  rows={2}
                  value={goalInput}
                  onChange={(event) => setGoalInput(event.target.value)}
                  className="w-full resize-none rounded-md px-2 py-1.5 text-[10px] outline-none"
                  style={{
                    background: 'var(--bg-primary)',
                    border: '1px solid var(--border-primary)',
                    color: 'var(--text-primary)',
                  }}
                />
              </label>
              <div className="grid grid-cols-[minmax(0,1fr)_28px] gap-1">
                <label className="min-w-0">
                  <span className="sr-only">修改原因</span>
                  <input
                    value={goalReasonInput}
                    onChange={(event) => setGoalReasonInput(event.target.value)}
                    placeholder="修改原因"
                    className="h-7 w-full min-w-0 rounded-md px-2 text-[10px] outline-none"
                    style={{
                      background: 'var(--bg-primary)',
                      border: '1px solid var(--border-primary)',
                      color: 'var(--text-primary)',
                    }}
                  />
                </label>
                <button
                  type="submit"
                  className="flex h-7 w-7 items-center justify-center rounded-md"
                  style={{ background: 'var(--bg-hover)', color: 'var(--text-primary)' }}
                  title="更新任务目标"
                  aria-label="更新任务目标"
                >
                  <Save size={12} />
                </button>
              </div>
              {goalError && <div style={{ color: 'var(--color-error)' }}>{goalError}</div>}
              {!planGoalCurrent && (
                <div className="text-[10px]" style={{ color: 'var(--color-warning)' }}>
                  任务图待绑定 Goal r{activeRun.goal_revision}
                </div>
              )}
            </form>
          )}

          {continuation?.enabled && (
            <div className="mb-2 space-y-1 text-[10px]" style={{ color: 'var(--text-tertiary)' }}>
              <div className="flex flex-wrap items-center gap-x-2 gap-y-0.5">
                <span>第 {currentTurn?.ordinal ?? continuation.next_turn_ordinal} 轮</span>
                <span>压缩 {continuation.compaction_count} 次</span>
                {activeCellCount > 0 && <span>后台命令 {activeCellCount}</span>}
              </div>
              {budgetLabels && <div>{budgetLabels.tokens}</div>}
              {budgetLabels && <div>{budgetLabels.time}</div>}
              <form
                className="grid grid-cols-[minmax(0,1fr)_minmax(0,1fr)_28px] gap-1 pt-1"
                onSubmit={(event) => {
                  event.preventDefault();
                  void applyBudgets();
                }}
              >
                <label className="min-w-0">
                  <span className="sr-only">Token 预算</span>
                  <input
                    type="number"
                    min={1}
                    step={1}
                    inputMode="numeric"
                    value={tokenBudgetInput}
                    onChange={(event) => setTokenBudgetInput(event.target.value)}
                    placeholder="Token 不限"
                    className="h-7 w-full min-w-0 rounded-md px-1.5 text-[10px] outline-none"
                    style={{
                      background: 'var(--bg-primary)',
                      border: '1px solid var(--border-primary)',
                      color: 'var(--text-primary)',
                    }}
                  />
                </label>
                <label className="min-w-0">
                  <span className="sr-only">时间预算（秒）</span>
                  <input
                    type="number"
                    min={1}
                    step={1}
                    inputMode="numeric"
                    value={timeBudgetInput}
                    onChange={(event) => setTimeBudgetInput(event.target.value)}
                    placeholder="秒数不限"
                    className="h-7 w-full min-w-0 rounded-md px-1.5 text-[10px] outline-none"
                    style={{
                      background: 'var(--bg-primary)',
                      border: '1px solid var(--border-primary)',
                      color: 'var(--text-primary)',
                    }}
                  />
                </label>
                <button
                  type="submit"
                  className="flex h-7 w-7 items-center justify-center rounded-md"
                  style={{ background: 'var(--bg-hover)', color: 'var(--text-primary)' }}
                  title="应用任务预算"
                  aria-label="应用任务预算"
                >
                  <Save size={12} />
                </button>
              </form>
              {budgetError && <div style={{ color: 'var(--color-error)' }}>{budgetError}</div>}
              {continuation.pause && (
                <div style={{ color: 'var(--color-warning)' }}>
                  {PAUSE_REASON_LABEL[continuation.pause.reason] ?? continuation.pause.reason}
                  {continuation.pause.detail ? ` · ${continuation.pause.detail}` : ''}
                </div>
              )}
            </div>
          )}

          {recoveryBlockers.length > 0 && (
            <div
              className="mb-2 space-y-2 rounded-md px-2 py-2"
              style={{ background: 'var(--bg-hover)', border: '1px solid var(--color-warning)' }}
            >
              <div
                className="flex items-center gap-1 text-[10px] font-medium"
                style={{ color: 'var(--color-warning)' }}
              >
                <AlertTriangle size={11} /> 需要确认恢复操作
              </div>
              {recoveryBlockers.map((blocker) => {
                const todo = todos.find((item) => item.task_id === blocker.task_id);
                return (
                  <div key={blocker.task_id} className="space-y-1">
                    <div
                      className="truncate text-[10px] font-medium"
                      style={{ color: 'var(--text-primary)' }}
                      title={todo?.title ?? blocker.task_id}
                    >
                      {todo?.title ?? blocker.task_id}
                    </div>
                    <div
                      className="text-[9px] leading-snug"
                      style={{ color: 'var(--text-secondary)' }}
                    >
                      {blocker.reason}
                      {blocker.tool_name ? ` · ${blocker.tool_name}` : ''}
                    </div>
                    <div className="grid grid-cols-2 gap-1">
                      <button
                        onClick={() =>
                          void performAction('重试恢复任务', () =>
                            retryBlockedTask(blocker.task_id, controlTarget)
                          )
                        }
                        disabled={!!pendingAction}
                        className="flex items-center justify-center gap-1 rounded-md px-2 py-1 text-[10px]"
                        style={{ background: 'var(--bg-primary)', color: 'var(--text-primary)' }}
                        title="确认工作区状态后重新执行"
                      >
                        <RotateCcw size={10} /> 重试
                      </button>
                      <button
                        onClick={() =>
                          void performAction('跳过恢复任务', () =>
                            resolveRecoveryTask(blocker.task_id, 'skip', controlTarget)
                          )
                        }
                        disabled={!!pendingAction}
                        className="flex items-center justify-center gap-1 rounded-md px-2 py-1 text-[10px]"
                        style={{ background: 'var(--bg-primary)', color: 'var(--text-secondary)' }}
                        title="保留当前工作区并跳过该任务"
                      >
                        <SkipForward size={10} /> 跳过
                      </button>
                    </div>
                  </div>
                );
              })}
            </div>
          )}

          {completionGate && !completionGateCurrent && (
            <p role="status" className="eko-task-history-notice">
              验收报告尚未匹配当前运行、Goal或计划版本，请刷新后确认；旧通过状态不计当前完成。
            </p>
          )}
          {completionGate && completionGateCurrent && completionGate.requirements.length > 0 && (
            <div className="mb-2 border-t border-[var(--border-primary)] pt-2">
              <div className="mb-1 flex items-center justify-between text-[10px]">
                <span className="font-medium" style={{ color: 'var(--text-tertiary)' }}>
                  Goal 验收 · r{completionGate.goal_revision}
                </span>
                <span
                  style={{
                    color: completionGate.ready ? 'var(--color-success)' : 'var(--color-warning)',
                  }}
                >
                  {completionGate.ready ? '通过' : `${completionGate.blockers.length} 项阻塞`}
                </span>
              </div>
              <div className="space-y-1" data-testid="completion-gate-requirements">
                {completionGate.requirements.map((assessment) => {
                  const requirement = assessment.requirement;
                  const taskStatus = todos.find(
                    (todo) => todo.task_id === requirement.task_id
                  )?.status;
                  const canConfirmSkip =
                    taskStatus === 'skipped' && assessment.status !== 'skipped' && planGoalCurrent;
                  const editingSkip = requirementSkipId === requirement.requirement_id;
                  return (
                    <div key={requirement.requirement_id} className="min-w-0 px-1 py-0.5">
                      <div className="flex min-w-0 items-center gap-1">
                        <span
                          className="min-w-0 flex-1 truncate text-[10px]"
                          style={{ color: 'var(--text-primary)' }}
                          title={requirement.description}
                        >
                          {requirement.title}
                        </span>
                        <span
                          className="shrink-0 text-[9px]"
                          style={{ color: 'var(--text-tertiary)' }}
                        >
                          {REQUIREMENT_LABEL[assessment.status] ?? assessment.status}
                        </span>
                        {canConfirmSkip && !editingSkip && (
                          <button
                            type="button"
                            className="rounded-md p-0.5 hover:bg-[var(--bg-hover)]"
                            title="确认跳过该 Goal requirement"
                            aria-label="确认跳过该 Goal requirement"
                            onClick={() => {
                              setRequirementSkipId(requirement.requirement_id);
                              setRequirementSkipReason('');
                            }}
                          >
                            <SkipForward size={10} style={{ color: 'var(--color-warning)' }} />
                          </button>
                        )}
                      </div>
                      {editingSkip && (
                        <div className="mt-1 grid grid-cols-[minmax(0,1fr)_24px_24px] gap-1">
                          <input
                            autoFocus
                            value={requirementSkipReason}
                            onChange={(event) => setRequirementSkipReason(event.target.value)}
                            placeholder="跳过原因"
                            className="h-6 min-w-0 rounded-md px-1.5 text-[9px] outline-none"
                            style={{
                              background: 'var(--bg-primary)',
                              border: '1px solid var(--border-primary)',
                              color: 'var(--text-primary)',
                            }}
                          />
                          <button
                            type="button"
                            disabled={!requirementSkipReason.trim()}
                            className="flex h-6 w-6 items-center justify-center rounded-md"
                            style={{ background: 'var(--bg-hover)', color: 'var(--text-primary)' }}
                            title="保存跳过原因"
                            aria-label="保存跳过原因"
                            onClick={() => void confirmRequirementSkip()}
                          >
                            <Save size={10} />
                          </button>
                          <button
                            type="button"
                            className="flex h-6 w-6 items-center justify-center rounded-md"
                            style={{ background: 'var(--bg-hover)', color: 'var(--text-tertiary)' }}
                            title="取消"
                            aria-label="取消"
                            onClick={() => {
                              setRequirementSkipId(null);
                              setRequirementSkipReason('');
                            }}
                          >
                            <X size={10} />
                          </button>
                        </div>
                      )}
                    </div>
                  );
                })}
              </div>
            </div>
          )}

          <div className="mb-2">
            {hasMoreEvents ? (
              <p className="text-xs text-[var(--text-tertiary)]">
                执行记录未读完，用量汇总待确认。
              </p>
            ) : (
              <CacheUsageCard summary={usageSummary} compact />
            )}
          </div>

          {runId && (
            <button
              onClick={() =>
                void performAction('刷新', () => refresh(activeRun.workspace_id, runId))
              }
              disabled={!!pendingAction}
              className="ml-auto flex items-center gap-1 text-[10px]"
              style={{ color: 'var(--text-tertiary)' }}
            >
              <RefreshCw size={11} /> 刷新
            </button>
          )}
        </div>
      </details>
    </section>
  );
}

/**
 * Interrupt prompt dialog — shown when a new message arrives while a run is
 * in-progress. Lets the user choose: resume, edit-and-resume, or abandon.
 */
export function InterruptPromptDialog() {
  const interruptPrompt = useTaskRuntimeStore((s) => s.interruptPrompt);
  const dismiss = useTaskRuntimeStore((s) => s.dismissInterruptPrompt);

  if (!interruptPrompt) return null;

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center"
      style={{ background: 'rgba(0,0,0,0.4)' }}
    >
      <div
        className="mx-4 w-full max-w-sm rounded-lg p-4 shadow-[var(--shadow-lg)]"
        style={{ background: 'var(--bg-primary)', border: '1px solid var(--border-primary)' }}
      >
        <div className="mb-3 flex items-center justify-between">
          <span className="text-sm font-medium" style={{ color: 'var(--text-primary)' }}>
            任务正在执行中
          </span>
          <button onClick={dismiss} className="rounded-md p-1 hover:bg-[var(--bg-hover)]">
            <X size={14} style={{ color: 'var(--text-tertiary)' }} />
          </button>
        </div>
        <p className="mb-1 text-xs" style={{ color: 'var(--text-secondary)' }}>
          当前有一个正在执行的任务计划：
        </p>
        <p className="mb-3 truncate text-xs font-medium" style={{ color: 'var(--text-primary)' }}>
          {interruptPrompt.goal}
        </p>
        <p className="mb-3 text-xs" style={{ color: 'var(--text-secondary)' }}>
          你想怎么处理？
        </p>
        <div className="flex flex-col gap-2">
          <button
            className="flex items-center gap-2 rounded-md px-3 py-2 text-xs hover:bg-[var(--bg-hover)]"
            style={{ border: '1px solid var(--border-primary)', color: 'var(--text-primary)' }}
            onClick={() => {
              void interruptPrompt.resolve('continue');
            }}
          >
            <Play size={12} /> 继续执行旧计划
          </button>
          <button
            className="flex items-center gap-2 rounded-md px-3 py-2 text-xs hover:bg-[var(--bg-hover)]"
            style={{ border: '1px solid var(--border-primary)', color: 'var(--text-primary)' }}
            onClick={async () => {
              await interruptPrompt.resolve('edit');
            }}
          >
            <Edit3 size={12} /> 编辑计划后继续
          </button>
          <button
            className="flex items-center gap-2 rounded-md px-3 py-2 text-xs hover:bg-[var(--bg-hover)]"
            style={{ border: '1px solid var(--border-primary)', color: 'var(--color-error)' }}
            onClick={async () => {
              await interruptPrompt.resolve('cancel_and_start');
            }}
          >
            <Trash2 size={12} /> 废弃旧计划，开始新任务
          </button>
        </div>
      </div>
    </div>
  );
}
