//! TaskRuntime panels.
//!
//! Renders the structured state of a complex-task run from the canonical
//! SQLite store (via taskRuntimeStore), NOT from regex-scanned chat messages.
//! Shows: run header, plan + approval actions (when AwaitingPlanApproval),
//! todo list with live status, parallel worker view, and artifacts.
//!
//! The compact panel is mounted inside RightRail; the full detail panel is
//! mounted in the main chat/work area.

import { useEffect, useMemo, useState, type ReactNode } from 'react';
import {
  CheckCircle2,
  Circle,
  Loader2,
  AlertCircle,
  ListTodo,
  FileText,
  Play,
  XCircle,
  RefreshCw,
  ChevronRight,
  ChevronDown,
  Brain,
  Wrench,
  MessageSquareText,
  ShieldCheck,
  Workflow,
  Sparkles,
  Copy,
  Gauge,
  AlertTriangle,
} from 'lucide-react';
import { useTaskRuntimeStore } from '../../stores/taskRuntimeStore';
import { useConversationStore } from '../../stores/conversationStore';
import { useUiStore } from '../../stores/uiStore';
import { taskRuntimeApi } from '../../api/endpoints';
import {
  useWorkerTraceStore,
  type WorkerTraceEvent,
  type WorkerTraceState,
  type WorkerTraceStatus,
} from '../../stores/workerTraceStore';
import type { TodoStatus, PlanTaskKind, TaskRun, TaskPlan, TodoItem } from '../../generated';
import type { RouteExplanation } from '../../stores/taskRuntimeStore';

const STATUS_LABEL: Record<string, string> = {
  pending: '待处理',
  planning: '规划中',
  awaiting_plan_approval: '待确认计划',
  ready: '就绪',
  running: '执行中',
  waiting_approval: '等待审批',
  waiting_input: '等待输入',
  suspended: '已挂起',
  cancelling: '取消中',
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
  skipped: '已跳过',
};

function statusColor(status: string): string {
  if (['completed'].includes(status)) return 'var(--color-success)';
  if (['running', 'planning', 'ready'].includes(status)) return 'var(--color-info)';
  if (['failed', 'cancelled'].includes(status)) return 'var(--color-error)';
  if (['suspended', 'blocked', 'waiting_approval', 'awaiting_plan_approval', 'waiting_input', 'cancelling'].includes(status))
    return 'var(--color-warning)';
  return 'var(--text-tertiary)';
}

function TodoIcon({ status }: { status: string }) {
  switch (status) {
    case 'completed':
      return <CheckCircle2 size={14} style={{ color: 'var(--color-success)' }} />;
    case 'running':
      return <Loader2 size={14} className="animate-spin" style={{ color: 'var(--color-info)' }} />;
    case 'failed':
      return <AlertCircle size={14} style={{ color: 'var(--color-error)' }} />;
    case 'skipped':
    case 'blocked':
      return <Circle size={14} style={{ color: 'var(--text-tertiary)' }} />;
    default:
      return <Circle size={14} style={{ color: 'var(--text-tertiary)' }} />;
  }
}

function kindLabel(kind: string): string {
  const map: Record<string, string> = {
    read_only_review: '审查',
    investigation: '调研',
    test_plan: '测试规划',
    implementation: '实现',
    debugging: '调试',
    review: '复核',
    summary: '总结',
    verification: '验证',
  };
  return map[kind] ?? kind;
}

function routeLabel(route?: string): string {
  const map: Record<string, string> = {
    normal_chat: '普通对话',
    plan_only: '只规划',
    complex_runtime: '任务运行时',
    parallel_readonly_delegation: '只读并行',
    background_task: '后台任务',
    direct_edit: '直接编辑',
  };
  return route ? map[route] ?? route : '未知';
}

function modeLabel(mode?: string): string {
  const map: Record<string, string> = {
    auto: 'Auto',
    chat: 'Chat',
    task: 'Task',
  };
  return mode ? map[mode] ?? mode : 'Auto';
}

function permissionLabel(mode?: string): string {
  const map: Record<string, string> = {
    default: '默认审批',
    'auto-edit': '自动编辑',
    'full-auto': '全自动',
    strict: '严格确认',
  };
  return mode ? map[mode] ?? mode : '默认审批';
}

function domainProfileLabel(profile?: string): string {
  const map: Record<string, string> = {
    general: '通用',
    ai_coding: 'AI Coding',
    data_analysis: '数据分析',
    academic_research: '学术研究',
    medical_research: '医学研究',
  };
  return profile ? map[profile] ?? profile : '通用';
}

function isReadOnlyKind(kind: PlanTaskKind): boolean {
  return ['read_only_review', 'investigation', 'test_plan', 'review', 'summary'].includes(kind);
}

function uniqueValues(values: Array<string | null | undefined>): string[] {
  return [...new Set(values.filter((value): value is string => Boolean(value && value.trim())))];
}

function deriveRouteExplanation(
  run: TaskRun,
  plan: TaskPlan | null,
  todos: TodoItem[],
  workers: WorkerTraceState[],
  liveExplanation: RouteExplanation | null
): RouteExplanation {
  const plannedTasks = plan?.tasks ?? [];
  const readOnlyCount = plannedTasks.filter((task) => isReadOnlyKind(task.kind)).length;
  const allReadOnly = plannedTasks.length > 0 && readOnlyCount === plannedTasks.length;
  const workerNames = uniqueValues([
    ...workers.map((worker) => worker.agentName),
    ...todos.map((todo) => todo.owner_agent),
    ...plannedTasks.map((task) => task.agent_role),
  ]);
  if (liveExplanation && liveExplanation.runId === run.run_id) {
    const plannedWorkers = uniqueValues([
      ...workerNames,
      ...(liveExplanation.plannedWorkers ?? []),
    ]);
    return {
      ...liveExplanation,
      goal: liveExplanation.goal ?? run.goal,
      domainProfile: liveExplanation.domainProfile ?? run.domain_profile,
      plannedWorkers,
      suggestedWorkers: liveExplanation.suggestedWorkers ?? [],
      activeSkills: liveExplanation.activeSkills ?? [],
    };
  }

  const hasParallelWorkers = workerNames.length > 1 || workers.length > 1;
  const route = allReadOnly && hasParallelWorkers ? 'parallel_readonly_delegation' : 'complex_runtime';
  const autoExecute = allReadOnly && route === 'parallel_readonly_delegation';

  return {
    runId: run.run_id,
    goal: run.goal,
    domainProfile: run.domain_profile,
    route,
    routeReason: [
      '实时 plan_ready 路由事件不可用，以下说明根据已保存的运行记录推断。',
      `${plannedTasks.length || todos.length} 个计划任务，${readOnlyCount} 个只读任务，${workerNames.length} 个 worker/角色。`,
      allReadOnly && hasParallelWorkers
        ? '任务形态符合只读并行：可以拆给多个 worker 并发探索、审查和汇总。'
        : '任务需要 TaskRuntime 维护计划、状态和审批，而不是普通 chat 串行回复。',
    ].join(' '),
    confidence: undefined,
    autoExecute,
    plannedWorkers: workerNames,
    suggestedWorkers: workerNames,
    activeSkills: [],
    routeSignals: allReadOnly ? ['saved_plan_all_read_only'] : ['saved_task_runtime_run'],
    classificationSignals: [`domain:${domainProfileLabel(run.domain_profile)}`],
  };
}

function routeWorkerCount(explanation: RouteExplanation, traceWorkerCount: number): number {
  return (
    explanation.plannedWorkers?.length ||
    traceWorkerCount ||
    explanation.suggestedWorkers?.length ||
    0
  );
}

function routeWorkerNames(explanation: RouteExplanation): string[] {
  return explanation.plannedWorkers?.length
    ? explanation.plannedWorkers
    : explanation.suggestedWorkers ?? [];
}

function CompactTag({ children }: { children: ReactNode }) {
  return (
    <span
      className="rounded px-1.5 py-0.5 text-[9px]"
      style={{ background: 'var(--bg-hover)', color: 'var(--text-tertiary)' }}
    >
      {children}
    </span>
  );
}

function workerStatusLabel(status: WorkerTraceStatus): string {
  const map: Record<WorkerTraceStatus, string> = {
    planned: '已规划',
    running: '运行中',
    completed: '已完成',
    failed: '失败',
    cancelled: '已取消',
  };
  return map[status];
}

function payloadValue(event: WorkerTraceEvent, key: string): string | undefined {
  if (!event.payload || typeof event.payload !== 'object' || Array.isArray(event.payload)) {
    return undefined;
  }
  const value = (event.payload as Record<string, unknown>)[key];
  if (typeof value === 'string') return value;
  if (typeof value === 'number' || typeof value === 'boolean') return String(value);
  return undefined;
}

function payloadBool(event: WorkerTraceEvent, key: string): boolean | undefined {
  if (!event.payload || typeof event.payload !== 'object' || Array.isArray(event.payload)) {
    return undefined;
  }
  const value = (event.payload as Record<string, unknown>)[key];
  return typeof value === 'boolean' ? value : undefined;
}

function payloadNumber(event: WorkerTraceEvent, key: string): number {
  if (!event.payload || typeof event.payload !== 'object' || Array.isArray(event.payload)) {
    return 0;
  }
  const value = (event.payload as Record<string, unknown>)[key];
  if (typeof value === 'number' && Number.isFinite(value)) return value;
  if (typeof value === 'string') {
    const parsed = Number(value);
    return Number.isFinite(parsed) ? parsed : 0;
  }
  return 0;
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

function cacheUsageFromEvents(events: WorkerTraceEvent[]): CacheUsageSummary {
  const usageEvents = events.filter((event) => event.event_type === 'worker_llm_usage');
  const models = uniqueValues(usageEvents.map((event) => payloadValue(event, 'model')));
  return usageEvents.reduce<CacheUsageSummary>(
    (summary, event) => {
      const reported = payloadBool(event, 'usage_reported');
      summary.calls += 1;
      if (!reported) {
        // Unreported usage: count but don't pollute main token statistics.
        summary.missingUsage += 1;
      } else {
        summary.inputTokens += payloadNumber(event, 'prompt_tokens');
        summary.outputTokens += payloadNumber(event, 'completion_tokens');
        summary.totalTokens += payloadNumber(event, 'total_tokens');
        summary.cachedPromptTokens += payloadNumber(event, 'cached_prompt_tokens');
        summary.cacheCreationPromptTokens += payloadNumber(event, 'cache_creation_prompt_tokens');
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

function cacheUsageForWorkers(workers: WorkerTraceState[]): CacheUsageSummary {
  return cacheUsageFromEvents(workers.flatMap((worker) => worker.events));
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

  if (rate != null && summary.inputTokens > 0 && summary.cachedPromptTokens === 0 && summary.missingUsage < summary.calls) {
    diagnostics.push({
      severity: 'warn',
      label: '没有 cache read',
      detail: 'provider 已返回 usage，但 cached prompt tokens 为 0。优先检查 system prefix、tools 定义、worker prompt 和动态上下文是否每轮变化。',
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

  if (summary.cacheCreationPromptTokens > summary.cachedPromptTokens && summary.cacheCreationPromptTokens > 0) {
    diagnostics.push({
      severity: 'info',
      label: 'cache write 高于 read',
      detail: '本轮更多是在创建缓存而非读取缓存。连续重复同类任务时，如果 read 仍不升高，再检查前缀稳定性。',
    });
  }

  if (diagnostics.length === 0 && summary.calls > 0) {
    diagnostics.push({
      severity: 'info',
      label: '数据不足',
      detail: '当前 usage 数据不足以判断命中低的原因。重复同模型同提示词任务后再观察 cached tokens 和 read rate。',
    });
  }

  return diagnostics;
}

function diagnosticColor(severity: CacheDiagnostic['severity']): string {
  if (severity === 'good') return 'var(--color-success)';
  if (severity === 'warn') return 'var(--color-warning)';
  return 'var(--text-tertiary)';
}

function CacheUsageCard({
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
    <div
      className={`rounded-lg border ${compact ? 'p-2' : 'p-3'}`}
      style={{ borderColor: 'var(--border-primary)', background: 'var(--bg-secondary)' }}
    >
      <div className="mb-2 flex items-center justify-between gap-2">
        <div className="flex min-w-0 items-center gap-1.5">
          <Gauge size={compact ? 12 : 14} style={{ color: 'var(--accent)' }} />
          <span className={compact ? 'text-[10px] font-medium' : 'text-[12px] font-medium'} style={{ color: 'var(--text-primary)' }}>
            Token / Cache
          </span>
        </div>
        <span className="shrink-0 text-[9px]" style={{ color: 'var(--text-tertiary)' }}>
          {summary.calls} LLM call{summary.calls > 1 ? 's' : ''}
          {summary.missingUsage > 0 && (
            <span style={{ color: 'var(--color-warning)' }}>
              {' '}
              ({summary.missingUsage} 未上报)
            </span>
          )}
        </span>
      </div>
      <div className="grid grid-cols-3 gap-1.5">
        <MetricCell label="Input" value={summary.inputTokens.toLocaleString()} valueClass={valueClass} labelClass={labelClass} />
        <MetricCell label="Output" value={summary.outputTokens.toLocaleString()} valueClass={valueClass} labelClass={labelClass} />
        <MetricCell label="Cached" value={summary.cachedPromptTokens.toLocaleString()} valueClass={valueClass} labelClass={labelClass} />
        <MetricCell label="Cache write" value={summary.cacheCreationPromptTokens.toLocaleString()} valueClass={valueClass} labelClass={labelClass} />
        <MetricCell label="Read rate" value={rate == null ? 'unknown' : `${(rate * 100).toFixed(1)}%`} valueClass={valueClass} labelClass={labelClass} />
        <MetricCell label="Missing usage" value={summary.missingUsage.toLocaleString()} valueClass={valueClass} labelClass={labelClass} />
      </div>
      {summary.models.length > 0 && (
        <div className="mt-2 truncate text-[9px]" style={{ color: 'var(--text-tertiary)' }} title={summary.models.join(', ')}>
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
          <div className="flex items-center gap-1 text-[10px] font-medium" style={{ color: 'var(--text-secondary)' }}>
            <AlertTriangle size={11} />
            缓存诊断
          </div>
          {diagnostics.map((diagnostic) => (
            <div
              key={`${diagnostic.label}:${diagnostic.detail}`}
              className="rounded-md px-2 py-1.5 text-[10px] leading-snug"
              style={{ background: 'var(--bg-hover)', color: 'var(--text-secondary)' }}
            >
              <div className="mb-0.5 font-medium" style={{ color: diagnosticColor(diagnostic.severity) }}>
                {diagnostic.label}
              </div>
              <div>{diagnostic.detail}</div>
            </div>
          ))}
        </div>
      )}
    </div>
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

function RuntimeStoryStep({
  icon,
  title,
  meta,
  children,
}: {
  icon: ReactNode;
  title: string;
  meta?: ReactNode;
  children: ReactNode;
}) {
  return (
    <div className="grid grid-cols-[26px_1fr] gap-3">
      <div className="flex flex-col items-center">
        <div
          className="flex h-6 w-6 items-center justify-center rounded-full border"
          style={{ borderColor: 'var(--border-primary)', background: 'var(--bg-secondary)', color: 'var(--accent)' }}
        >
          {icon}
        </div>
        <div className="mt-2 min-h-4 flex-1 border-l" style={{ borderColor: 'var(--border-primary)' }} />
      </div>
      <div className="min-w-0 pb-5">
        <div className="mb-2 flex min-w-0 items-center justify-between gap-2">
          <div className="truncate text-[13px] font-medium" style={{ color: 'var(--text-primary)' }}>
            {title}
          </div>
          {meta && (
            <div className="shrink-0 text-[10px]" style={{ color: 'var(--text-tertiary)' }}>
              {meta}
            </div>
          )}
        </div>
        {children}
      </div>
    </div>
  );
}

function copyToClipboard(text: string) {
  if (typeof navigator === 'undefined' || !navigator.clipboard) return;
  void navigator.clipboard.writeText(text);
}

function workerResult(worker: WorkerTraceState): string {
  const completed = [...worker.events]
    .reverse()
    .find((event) => event.event_type === 'worker_completed');
  const summary = completed ? payloadValue(completed, 'summary') : undefined;
  if (summary) return summary;

  const output = worker.events
    .filter((event) => event.event_type === 'worker_token_delta')
    .map((event) => payloadValue(event, 'content') ?? '')
    .join('')
    .trim();
  return output;
}

function workerThinking(worker: WorkerTraceState): string {
  return worker.events
    .filter((event) => event.event_type === 'worker_thinking_delta')
    .map((event) => payloadValue(event, 'content') ?? '')
    .join('')
    .trim();
}

function workerToolEvents(worker: WorkerTraceState): WorkerTraceEvent[] {
  return worker.events.filter(
    (event) => event.event_type === 'worker_tool_start' || event.event_type === 'worker_tool_result'
  );
}

function traceWorkerForTodo(todo: { owner_agent: string | null }, workers: WorkerTraceState[]) {
  if (!todo.owner_agent) return undefined;
  return workers.find((worker) => worker.agentName === todo.owner_agent);
}

function displayedTodoStatus(todo: { status: TodoStatus; owner_agent: string | null }, workers: WorkerTraceState[]): TodoStatus {
  const worker = traceWorkerForTodo(todo, workers);
  if (!worker) return todo.status;
  if (worker.status === 'completed' && todo.status !== ('completed' as TodoStatus)) {
    return 'completed' as TodoStatus;
  }
  if (worker.status === 'failed' && todo.status !== ('failed' as TodoStatus)) {
    return 'failed' as TodoStatus;
  }
  if (worker.status === 'cancelled' && todo.status !== ('skipped' as TodoStatus)) {
    return 'skipped' as TodoStatus;
  }
  return todo.status;
}

function finalRunResult(workers: WorkerTraceState[], todos: Array<{ title: string; summary: string | null }>): string | null {
  const summaryWorker =
    workers.find((worker) => worker.agentName === 'summary_writer') ??
    workers.find((worker) => /summary|synthesize/i.test(worker.title ?? worker.workerId));
  const workerSummary = summaryWorker ? workerResult(summaryWorker) : '';
  if (workerSummary) return workerSummary;

  const summarizedTodos = todos
    .filter((todo) => todo.summary && todo.summary.trim())
    .map((todo) => `- ${todo.title}: ${todo.summary}`);
  return summarizedTodos.length > 0 ? summarizedTodos.join('\n') : null;
}

function eventLabel(event: WorkerTraceEvent): { icon: ReactNode; label: string; detail?: string } {
  switch (event.event_type) {
    case 'worker_thinking_start':
      return {
        icon: <Brain size={11} style={{ color: 'var(--color-info)' }} />,
        label: '开始思考',
      };
    case 'worker_thinking_delta':
      return {
        icon: <Brain size={11} style={{ color: 'var(--color-info)' }} />,
        label: '思考',
        detail: payloadValue(event, 'content'),
      };
    case 'worker_thinking_end':
      return {
        icon: <Brain size={11} style={{ color: 'var(--text-tertiary)' }} />,
        label: '思考结束',
        detail: [
          payloadValue(event, 'prompt_tokens') && `in ${payloadValue(event, 'prompt_tokens')}`,
          payloadValue(event, 'completion_tokens') && `out ${payloadValue(event, 'completion_tokens')}`,
        ].filter(Boolean).join(' · ') || undefined,
      };
    case 'worker_llm_usage':
      return {
        icon: <Gauge size={11} style={{ color: 'var(--accent)' }} />,
        label: 'Token / Cache',
        detail: [
          payloadValue(event, 'model'),
          payloadValue(event, 'prompt_tokens') && `in ${payloadValue(event, 'prompt_tokens')}`,
          payloadValue(event, 'completion_tokens') && `out ${payloadValue(event, 'completion_tokens')}`,
          payloadValue(event, 'cached_prompt_tokens') && `cached ${payloadValue(event, 'cached_prompt_tokens')}`,
          payloadValue(event, 'cache_creation_prompt_tokens') && `write ${payloadValue(event, 'cache_creation_prompt_tokens')}`,
          payloadBool(event, 'usage_reported') === false && 'usage missing',
        ].filter(Boolean).join(' · ') || undefined,
      };
    case 'worker_tool_start':
      return {
        icon: <Wrench size={11} style={{ color: 'var(--color-warning)' }} />,
        label: `调用工具 ${payloadValue(event, 'name') ?? ''}`.trim(),
        detail: payloadValue(event, 'args'),
      };
    case 'worker_tool_result':
      return {
        icon: <Wrench size={11} style={{ color: payloadValue(event, 'success') === 'false' ? 'var(--color-error)' : 'var(--color-success)' }} />,
        label: `工具结果 ${payloadValue(event, 'name') ?? ''}`.trim(),
        detail: payloadValue(event, 'result'),
      };
    case 'worker_token_delta':
      return {
        icon: <MessageSquareText size={11} style={{ color: 'var(--text-tertiary)' }} />,
        label: '输出',
        detail: payloadValue(event, 'content'),
      };
    case 'worker_started':
      return {
        icon: <Loader2 size={11} className="animate-spin" style={{ color: 'var(--color-info)' }} />,
        label: '开始运行',
        detail: payloadValue(event, 'kind'),
      };
    case 'worker_completed':
      return {
        icon: <CheckCircle2 size={11} style={{ color: 'var(--color-success)' }} />,
        label: '完成',
        detail: payloadValue(event, 'summary'),
      };
    case 'worker_failed':
      return {
        icon: <AlertCircle size={11} style={{ color: 'var(--color-error)' }} />,
        label: '失败',
        detail: payloadValue(event, 'error'),
      };
    default:
      return {
        icon: <Circle size={11} style={{ color: 'var(--text-tertiary)' }} />,
        label: event.event_type,
      };
  }
}

function eventTime(timestamp: string): string {
  const date = new Date(timestamp);
  if (Number.isNaN(date.getTime())) return '';
  return date.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', second: '2-digit' });
}

function WorkerTraceRow({
  worker,
  expanded,
  onToggle,
  roomy = false,
}: {
  worker: WorkerTraceState;
  expanded: boolean;
  onToggle: () => void;
  roomy?: boolean;
}) {
  const latest = worker.events[worker.events.length - 1];
  const latestLabel = latest ? eventLabel(latest) : null;
  const thinking = workerThinking(worker);
  const tools = workerToolEvents(worker);
  const result = workerResult(worker);
  const usage = cacheUsageFromEvents(worker.events);
  const hasDetails = Boolean(worker.task || thinking || tools.length || result);
  const [copied, setCopied] = useState<string | null>(null);

  const copy = (label: string, text: string) => {
    copyToClipboard(text);
    setCopied(label);
    window.setTimeout(() => setCopied(null), 1600);
  };

  return (
    <div
      className={`rounded-lg ${roomy ? 'border p-3' : 'px-1.5 py-1'}`}
      style={{
        background: 'var(--bg-secondary)',
        borderColor: roomy ? 'var(--border-primary)' : undefined,
      }}
    >
      <button
        type="button"
        onClick={onToggle}
        className="flex w-full items-start gap-2 text-left"
      >
        <div className="mt-0.5">
          {expanded ? (
            <ChevronDown size={roomy ? 15 : 12} style={{ color: 'var(--text-tertiary)' }} />
          ) : (
            <ChevronRight size={roomy ? 15 : 12} style={{ color: 'var(--text-tertiary)' }} />
          )}
        </div>
        <div className="mt-0.5">
          {worker.status === 'running' ? (
            <Loader2 size={roomy ? 15 : 12} className="animate-spin" style={{ color: 'var(--color-info)' }} />
          ) : worker.status === 'completed' ? (
            <CheckCircle2 size={roomy ? 15 : 12} style={{ color: 'var(--color-success)' }} />
          ) : worker.status === 'failed' ? (
            <AlertCircle size={roomy ? 15 : 12} style={{ color: 'var(--color-error)' }} />
          ) : (
            <Circle size={roomy ? 15 : 12} style={{ color: 'var(--text-tertiary)' }} />
          )}
        </div>
        <div className="min-w-0 flex-1">
          <div className="flex min-w-0 items-center gap-1">
            <span
              className={`truncate font-medium ${roomy ? 'text-sm' : 'text-[11px]'}`}
              style={{ color: 'var(--text-primary)' }}
            >
              {worker.title ?? worker.task ?? worker.workerId}
            </span>
            <span
              className={`shrink-0 rounded px-1 ${roomy ? 'text-[10px]' : 'text-[9px]'}`}
              style={{ color: statusColor(worker.status), background: 'var(--bg-hover)' }}
            >
              {workerStatusLabel(worker.status)}
            </span>
          </div>
          <div
            className={`mt-0.5 flex min-w-0 items-center gap-1 ${roomy ? 'text-[11px]' : 'text-[9px]'}`}
            style={{ color: 'var(--text-tertiary)' }}
          >
            <span className="shrink-0 font-mono">{worker.agentName ?? 'worker'}</span>
            {latestLabel && (
              <>
                <span>·</span>
                <span className="truncate">{latestLabel.label}</span>
              </>
            )}
          </div>
        </div>
      </button>

      {expanded && (
        <div className="mt-3 space-y-3 border-l pl-3" style={{ borderColor: 'var(--border-primary)' }}>
          {!hasDetails && (
            <div className="text-[11px]" style={{ color: 'var(--text-tertiary)' }}>
              暂无详细事件。worker 已接入，但尚未收到思考、工具或输出流。
            </div>
          )}

          {worker.task && (
            <TraceSection
              title="提示词"
              icon={<MessageSquareText size={12} />}
              action={<TraceCopyButton copied={copied === 'prompt'} onClick={() => copy('prompt', worker.task ?? '')} />}
            >
              <ScrollableText text={worker.task} maxHeight={288} className="text-[11px]" />
            </TraceSection>
          )}

          <CacheUsageCard summary={usage} compact />

          {result && (
            <TraceSection
              title="结果"
              icon={<CheckCircle2 size={12} />}
              action={<TraceCopyButton copied={copied === 'result'} onClick={() => copy('result', result)} />}
            >
              <ScrollableText text={result} maxHeight={520} className="text-[11px]" />
            </TraceSection>
          )}

          {(thinking || tools.length > 0) && (
            <TraceSection
              title="中间过程"
              icon={<Brain size={12} />}
              action={thinking ? <TraceCopyButton copied={copied === 'thinking'} onClick={() => copy('thinking', thinking)} /> : undefined}
            >
              {thinking && (
                <ScrollableText text={thinking} maxHeight={288} className="mb-2 text-[11px]" />
              )}
              {tools.length > 0 && (
                <div className="space-y-1.5">
                  {tools.slice(-40).map((event) => {
                    const name = payloadValue(event, 'name') ?? 'tool';
                    const success = payloadBool(event, 'success');
                    const detail =
                      event.event_type === 'worker_tool_start'
                        ? payloadValue(event, 'args')
                        : payloadValue(event, 'result');
                    return (
                      <div
                        key={event.event_id}
                        className="rounded-md border px-2 py-1.5"
                        style={{ borderColor: 'var(--border-primary)', background: 'var(--bg-primary)' }}
                      >
                        <div className="flex items-center gap-1.5 text-[11px]" style={{ color: 'var(--text-primary)' }}>
                          <Wrench size={11} style={{ color: success === false ? 'var(--color-error)' : 'var(--color-warning)' }} />
                          <span className="font-mono">{name}</span>
                          <span className="ml-auto text-[9px]" style={{ color: 'var(--text-tertiary)' }}>
                            {event.event_type === 'worker_tool_start'
                              ? '调用'
                              : success === false
                                ? '失败'
                                : '完成'} · {eventTime(event.timestamp)}
                          </span>
                        </div>
                        {detail && (
                          <ScrollableText
                            text={detail}
                            maxHeight={144}
                            className="mt-1 text-[10px] leading-snug"
                            subtle
                          />
                        )}
                      </div>
                    );
                  })}
                </div>
              )}
            </TraceSection>
          )}
        </div>
      )}
    </div>
  );
}

function TraceSection({
  title,
  icon,
  action,
  children,
}: {
  title: string;
  icon: ReactNode;
  action?: ReactNode;
  children: ReactNode;
}) {
  return (
    <div>
      <div className="mb-1.5 flex items-center justify-between gap-2 text-[11px] font-medium" style={{ color: 'var(--text-tertiary)' }}>
        <div className="flex items-center gap-1.5">
          {icon}
          <span>{title}</span>
        </div>
        {action}
      </div>
      <div style={{ color: 'var(--text-secondary)' }}>{children}</div>
    </div>
  );
}

function TraceCopyButton({ copied, onClick }: { copied: boolean; onClick: () => void }) {
  return (
    <button
      type="button"
      onClick={(event) => {
        event.stopPropagation();
        onClick();
      }}
      className="flex items-center gap-1 rounded px-1.5 py-0.5 text-[10px]"
      style={{ color: 'var(--text-tertiary)', background: 'var(--bg-hover)' }}
    >
      <Copy size={10} />
      {copied ? '已复制' : '复制'}
    </button>
  );
}

function ScrollableText({
  text,
  maxHeight,
  className = '',
  subtle = false,
}: {
  text: string;
  maxHeight: number;
  className?: string;
  subtle?: boolean;
}) {
  const [expanded, setExpanded] = useState(false);
  const lineCount = text.split('\n').length;
  const charCount = Array.from(text).length;
  const effectiveMaxHeight = expanded ? Math.max(maxHeight * 2, 720) : maxHeight;

  return (
    <div className="min-w-0">
      <div className="mb-1 flex items-center justify-between gap-2">
        <span className="text-[10px]" style={{ color: 'var(--text-tertiary)' }}>
          {expanded ? `已展开滚动区域 · ${lineCount} 行 · ${charCount} 字` : `区域内滚动查看 · ${lineCount} 行 · ${charCount} 字`}
        </span>
        <button
          type="button"
          onClick={(event) => {
            event.stopPropagation();
            setExpanded((value) => !value);
          }}
          className="shrink-0 rounded px-1.5 py-0.5 text-[10px]"
          style={{ color: 'var(--text-tertiary)', background: 'var(--bg-hover)' }}
        >
          {expanded ? '收起' : '展开全文'}
        </button>
      </div>
      <div
        tabIndex={0}
        role="region"
        className={`min-w-0 overflow-y-auto whitespace-pre-wrap break-words rounded-md p-2 leading-relaxed ${className}`}
        style={{
          background: subtle ? 'transparent' : 'var(--bg-primary)',
          color: subtle ? 'var(--text-tertiary)' : 'var(--text-secondary)',
          border: subtle ? 'none' : '1px solid var(--border-secondary)',
          maxHeight: `${effectiveMaxHeight}px`,
          overflowX: 'auto',
          WebkitOverflowScrolling: 'touch',
        }}
      >
        {text}
      </div>
    </div>
  );
}

export function TaskRuntimePanel() {
  const activeId = useConversationStore((s) => s.activeId);
  const traceWorkers = useWorkerTraceStore((s) => s.workers);
  const { activeRun, plan, todos, routeExplanation, loadByConversation, refresh } =
    useTaskRuntimeStore();

  useEffect(() => {
    if (activeId) loadByConversation(activeId);
  }, [activeId, loadByConversation]);

  const visibleTraceWorkers = useMemo(
    () =>
      activeRun
        ? Object.values(traceWorkers)
            .filter((worker) => worker.runId === activeRun.run_id)
            .sort((a, b) => (a.startedAt ?? '').localeCompare(b.startedAt ?? ''))
        : [],
    [activeRun, traceWorkers]
  );

  if (!activeRun && !routeExplanation) return null;

  // Detailed execution timeline is now in ConversationTimeline (main panel).
  // This right-rail panel serves as a compact status summary only.
  const runId = activeRun?.run_id ?? routeExplanation?.runId;
  const usageSummary = cacheUsageForWorkers(visibleTraceWorkers);
  const completedCount = todos.filter(
    (t) => displayedTodoStatus(t, visibleTraceWorkers) === ('completed' as TodoStatus)
  ).length;

  return (
    <section className="border-b border-[var(--border-primary)] px-3 py-2.5">
      <div className="mb-2 flex items-center justify-between">
        <div className="flex min-w-0 items-center gap-1.5">
          <ListTodo size={13} style={{ color: 'var(--accent)' }} />
          <span className="truncate text-xs font-medium" style={{ color: 'var(--text-primary)' }}>
            任务运行
          </span>
        </div>
        <span
          className="rounded px-1.5 py-0.5 text-[10px] font-medium"
          style={{
            color: activeRun ? statusColor(activeRun.status) : 'var(--text-tertiary)',
            background: 'var(--bg-hover)',
          }}
        >
          {activeRun ? STATUS_LABEL[activeRun.status] ?? activeRun.status : '连接中'}
        </span>
      </div>

      <div
        className="mb-2 truncate rounded-md px-2 py-1.5 text-[11px]"
        style={{ background: 'var(--bg-secondary)', color: 'var(--text-secondary)' }}
        title={activeRun?.goal ?? routeExplanation?.goal}
      >
        {activeRun?.goal ?? routeExplanation?.goal ?? '正在读取任务'}
      </div>

      <div className="mb-2">
        <CacheUsageCard summary={usageSummary} compact />
      </div>

      {todos.length > 0 && (
        <div className="mb-2">
          <div className="mb-1 flex items-center justify-between">
            <span className="text-[10px] font-medium" style={{ color: 'var(--text-tertiary)' }}>
              任务列表
            </span>
            <span className="text-[10px]" style={{ color: 'var(--text-tertiary)' }}>
              {completedCount}/{todos.length}
            </span>
          </div>
          <div className="space-y-0.5">
            {todos.map((todo) => {
              const task = plan?.tasks.find((t) => t.id === todo.task_id);
              const status = displayedTodoStatus(todo, visibleTraceWorkers);
              return (
                <div
                  key={todo.id}
                  className="flex items-start gap-1.5 rounded px-1.5 py-1"
                  style={{ background: 'var(--bg-secondary)' }}
                >
                  <div className="mt-0.5">
                    <TodoIcon status={status} />
                  </div>
                  <div className="min-w-0 flex-1">
                    <div className="truncate text-[11px]" style={{ color: 'var(--text-primary)' }} title={todo.title}>
                      {todo.title}
                    </div>
                    <div className="flex min-w-0 items-center gap-1 text-[9px]" style={{ color: 'var(--text-tertiary)' }}>
                      {task && <span className="rounded px-1" style={{ background: 'var(--bg-hover)' }}>{kindLabel(task.kind)}</span>}
                      {todo.owner_agent && <span className="truncate">· {todo.owner_agent}</span>}
                      <span>· {TODO_LABEL[status] ?? status}</span>
                    </div>
                  </div>
                </div>
              );
            })}
          </div>
        </div>
      )}

      {visibleTraceWorkers.length > 0 && (
        <div className="mb-2">
          <div className="mb-1 flex items-center justify-between">
            <span className="text-[10px] font-medium" style={{ color: 'var(--text-tertiary)' }}>
              Worker 状态
            </span>
            <span className="text-[10px]" style={{ color: 'var(--text-tertiary)' }}>
              {visibleTraceWorkers.filter((w) => w.status === 'running').length} 运行中
            </span>
          </div>
          <div className="space-y-0.5">
            {visibleTraceWorkers.map((worker) => (
              <div
                key={`${worker.runId}:${worker.workerId}`}
                className="flex items-center gap-1.5 rounded px-1.5 py-1 text-[10px]"
                style={{ background: 'var(--bg-secondary)', color: 'var(--text-secondary)' }}
              >
                {worker.status === 'running' ? (
                  <Loader2 size={11} className="animate-spin" style={{ color: 'var(--color-info)' }} />
                ) : worker.status === 'completed' ? (
                  <CheckCircle2 size={11} style={{ color: 'var(--color-success)' }} />
                ) : worker.status === 'failed' ? (
                  <AlertCircle size={11} style={{ color: 'var(--color-error)' }} />
                ) : (
                  <Circle size={11} style={{ color: 'var(--text-tertiary)' }} />
                )}
                <span className="min-w-0 flex-1 truncate">{worker.title || worker.workerId}</span>
                <span className="shrink-0" style={{ color: 'var(--text-tertiary)' }}>
                  {workerStatusLabel(worker.status)}
                </span>
              </div>
            ))}
          </div>
        </div>
      )}

      {runId && (
        <button
          onClick={() => refresh(runId)}
          className="ml-auto flex items-center gap-1 text-[10px]"
          style={{ color: 'var(--text-tertiary)' }}
        >
          <RefreshCw size={11} /> 刷新
        </button>
      )}
    </section>
  );
}

export function TaskRuntimeMainPanel() {
  const [expandedWorkers, setExpandedWorkers] = useState<Record<string, boolean>>({});
  const [copiedFinalResult, setCopiedFinalResult] = useState(false);
  const [routeFeedbackMessage, setRouteFeedbackMessage] = useState<string | null>(null);
  const activeId = useConversationStore((s) => s.activeId);
  const setActiveSettingsTab = useUiStore((s) => s.setActiveSettingsTab);
  const traceWorkers = useWorkerTraceStore((s) => s.workers);
  const {
    activeRun,
    plan,
    todos,
    artifacts,
    awaitingApproval,
    error,
    routeExplanation,
    loadByConversation,
    refresh,
    approve,
    reject,
    execute,
    cancel,
  } = useTaskRuntimeStore();

  // Load the latest run for the active conversation when it changes.
  useEffect(() => {
    if (activeId) loadByConversation(activeId);
  }, [activeId, loadByConversation]);

  // Poll for updates while a run is in flight (every 3s). Stops polling when
  // the run reaches a terminal state. Suspended is NOT terminal — it can be
  // resumed by user intervention.
  const isTerminal = activeRun
    ? ['completed', 'cancelled', 'failed'].includes(activeRun.status)
    : true;
  useEffect(() => {
    if (!activeRun || isTerminal) return;
    const id = window.setInterval(() => refresh(activeRun.run_id), 3000);
    return () => window.clearInterval(id);
  }, [activeRun, isTerminal, refresh]);

  const visibleTraceWorkers = useMemo(
    () =>
      activeRun
        ? Object.values(traceWorkers)
            .filter((worker) => worker.runId === activeRun.run_id)
            .sort((a, b) => (a.startedAt ?? '').localeCompare(b.startedAt ?? ''))
        : [],
    [activeRun, traceWorkers]
  );

  if (!activeRun) {
    if (routeExplanation || error) {
      return (
        <section
          className="my-3 rounded-lg border p-4"
          style={{ borderColor: 'var(--border-primary)', background: 'var(--bg-primary)' }}
        >
          <div className="mb-2 flex items-center justify-between">
            <div className="flex items-center gap-1.5">
              <Workflow size={15} style={{ color: 'var(--accent)' }} />
              <span className="text-sm font-semibold" style={{ color: 'var(--text-primary)' }}>
                任务详情
              </span>
            </div>
            <span className="text-[10px]" style={{ color: 'var(--text-tertiary)' }}>
              正在连接运行状态
            </span>
          </div>

          {routeExplanation && (
            <div
              className="mb-2 rounded-md border p-2"
              style={{ borderColor: 'var(--border-primary)', background: 'var(--bg-secondary)' }}
            >
              <div className="mb-1.5 flex items-center justify-between gap-2">
                <div className="flex min-w-0 items-center gap-1.5">
                  <Workflow size={12} style={{ color: 'var(--accent)' }} />
                  <span className="truncate text-[11px] font-medium" style={{ color: 'var(--text-primary)' }}>
                    路由决策
                  </span>
                </div>
                {typeof routeExplanation.confidence === 'number' && (
                  <span className="text-[9px]" style={{ color: 'var(--text-tertiary)' }}>
                    {Math.round(routeExplanation.confidence * 100)}%
                  </span>
                )}
              </div>

              <div className="mb-1.5 flex flex-wrap gap-1">
                <CompactTag>模式 {modeLabel(routeExplanation.interactionMode)}</CompactTag>
                <CompactTag>路径 {routeLabel(routeExplanation.route)}</CompactTag>
                <CompactTag>{routeExplanation.autoExecute ? '自动执行' : '等待确认'}</CompactTag>
                <CompactTag>{permissionLabel(routeExplanation.permissionMode)}</CompactTag>
              </div>

              {routeExplanation.routeReason && (
                <div className="mb-1.5 text-[10px] leading-snug" style={{ color: 'var(--text-secondary)' }}>
                  {routeExplanation.routeReason}
                </div>
              )}

              {routeExplanation.approvalPolicy && (
                <div className="mb-1.5 flex gap-1.5 text-[10px] leading-snug" style={{ color: 'var(--text-tertiary)' }}>
                  <ShieldCheck size={11} className="mt-0.5 shrink-0" />
                  <span>{routeExplanation.approvalPolicy}</span>
                </div>
              )}

              {routeWorkerNames(routeExplanation).length > 0 && (
                <div className="flex flex-wrap gap-1">
                  {routeWorkerNames(routeExplanation).map((worker) => (
                    <CompactTag key={worker}>{worker}</CompactTag>
                  ))}
                </div>
              )}
            </div>
          )}

          {error && (
            <div
              className="mb-2 rounded px-2 py-1 text-[10px]"
              style={{ color: 'var(--color-error)', background: 'var(--bg-hover)' }}
            >
              {error}
            </div>
          )}

          {routeExplanation && (
            <button
              onClick={() => refresh(routeExplanation.runId)}
              className="flex w-full items-center justify-center gap-1 rounded px-2 py-1 text-[11px] font-medium"
              style={{ background: 'var(--bg-secondary)', color: 'var(--text-primary)' }}
            >
              <RefreshCw size={12} /> 重试读取运行状态
            </button>
          )}
        </section>
      );
    }
    return null;
  }

  const completedCount = todos.filter(
    (t) => displayedTodoStatus(t, visibleTraceWorkers) === ('completed' as TodoStatus)
  ).length;
  const runningWorkers = todos.filter((t) => t.status === ('running' as TodoStatus));
  const finalResult = finalRunResult(visibleTraceWorkers, todos);
  const effectiveRouteExplanation = deriveRouteExplanation(activeRun, plan, todos, visibleTraceWorkers, routeExplanation);
  const usageSummary = cacheUsageForWorkers(visibleTraceWorkers);

  const rememberRouteFeedback = async (targetRoute: string) => {
    const pattern = activeRun.goal.trim();
    if (!pattern || !effectiveRouteExplanation) return;
    const selectedWorkers =
      targetRoute === 'normal_chat' ? [] : routeWorkerNames(effectiveRouteExplanation);
    try {
      await taskRuntimeApi.upsertRouteFeedbackRule(
        pattern,
        targetRoute,
        `user corrected route from ${routeLabel(effectiveRouteExplanation.route)} to ${routeLabel(targetRoute)}`,
        selectedWorkers
      );
      setRouteFeedbackMessage(`已记住: 下次类似请求走 ${routeLabel(targetRoute)}`);
      window.setTimeout(() => setRouteFeedbackMessage(null), 2200);
    } catch (err) {
      setRouteFeedbackMessage(err instanceof Error ? err.message : String(err));
    }
  };

  return (
    <section
      className="my-3 rounded-lg border p-4"
      style={{ borderColor: 'var(--border-primary)', background: 'var(--bg-primary)' }}
    >
      {/* ── Run header ─────────────────────────────────────────────── */}
      <div className="mb-2 flex items-center justify-between">
        <div className="flex items-center gap-1.5">
          <ListTodo size={15} style={{ color: 'var(--accent)' }} />
          <span className="text-sm font-semibold" style={{ color: 'var(--text-primary)' }}>
            任务执行
          </span>
        </div>
        <span
          className="rounded px-1.5 py-0.5 text-[10px] font-medium"
          style={{
            color: statusColor(activeRun.status),
            background: 'var(--bg-hover)',
          }}
        >
          {STATUS_LABEL[activeRun.status] ?? activeRun.status}
        </span>
      </div>

      <div
        className="mb-2 rounded-md px-2 py-1.5 text-[11px] leading-snug"
        style={{ background: 'var(--bg-secondary)', color: 'var(--text-secondary)' }}
      >
        {activeRun.goal}
      </div>

      <div className="mt-4">
        {effectiveRouteExplanation && (
          <RuntimeStoryStep
            icon={<Sparkles size={14} />}
            title="路由决策"
            meta={`并发委派 ${routeWorkerCount(effectiveRouteExplanation, visibleTraceWorkers.length)} 个 worker`}
          >
            <div className="rounded-lg p-3" style={{ background: 'var(--bg-secondary)' }}>
              <div className="text-[11px] leading-relaxed" style={{ color: 'var(--text-secondary)' }}>
                {effectiveRouteExplanation.routeReason ||
                  `当前请求被识别为${routeLabel(effectiveRouteExplanation.route)}，适合拆分给多个 worker 并行处理。`}
              </div>
              <div className="mt-2 flex flex-wrap gap-1">
                {effectiveRouteExplanation.interactionMode && (
                  <CompactTag>模式 {modeLabel(effectiveRouteExplanation.interactionMode)}</CompactTag>
                )}
                <CompactTag>路径 {routeLabel(effectiveRouteExplanation.route)}</CompactTag>
                <CompactTag>{effectiveRouteExplanation.autoExecute ? '自动执行' : '等待确认'}</CompactTag>
                {effectiveRouteExplanation.permissionMode && (
                  <CompactTag>{permissionLabel(effectiveRouteExplanation.permissionMode)}</CompactTag>
                )}
                <CompactTag>领域 {domainProfileLabel(effectiveRouteExplanation.domainProfile)}</CompactTag>
                {effectiveRouteExplanation.activeSkills?.map((skill) => (
                  <CompactTag key={skill}>技能 {skill}</CompactTag>
                ))}
              </div>
              {effectiveRouteExplanation.approvalPolicy && (
                <div className="mt-2 flex gap-1.5 text-[11px] leading-snug" style={{ color: 'var(--text-tertiary)' }}>
                  <ShieldCheck size={12} className="mt-0.5 shrink-0" />
                  <span>{effectiveRouteExplanation.approvalPolicy}</span>
                </div>
              )}
              {(effectiveRouteExplanation.routeSignals.length > 0 || effectiveRouteExplanation.classificationSignals.length > 0) && (
                <div className="mt-2 flex flex-wrap gap-1">
                  {[...effectiveRouteExplanation.routeSignals, ...effectiveRouteExplanation.classificationSignals].map((signal) => (
                    <CompactTag key={signal}>{signal}</CompactTag>
                  ))}
                </div>
              )}
              <div className="mt-3 flex flex-wrap items-center gap-1.5">
                <button
                  onClick={() => void rememberRouteFeedback('normal_chat')}
                  className="rounded-md px-2 py-1 text-[11px] font-medium"
                  style={{ background: 'var(--bg-hover)', color: 'var(--text-secondary)' }}
                >
                  下次走 Chat
                </button>
                <button
                  onClick={() => void rememberRouteFeedback('parallel_readonly_delegation')}
                  className="rounded-md px-2 py-1 text-[11px] font-medium"
                  style={{ background: 'var(--bg-hover)', color: 'var(--text-secondary)' }}
                >
                  下次只读并行
                </button>
                <button
                  onClick={() => setActiveSettingsTab('routeFeedback')}
                  className="rounded-md px-2 py-1 text-[11px]"
                  style={{ color: 'var(--text-tertiary)' }}
                >
                  管理规则
                </button>
                {routeFeedbackMessage && (
                  <span className="text-[11px]" style={{ color: 'var(--text-tertiary)' }}>
                    {routeFeedbackMessage}
                  </span>
                )}
              </div>
            </div>
          </RuntimeStoryStep>
        )}

      {error && (
        <div
          className="mb-2 rounded px-2 py-1 text-[10px]"
          style={{ color: 'var(--color-error)', background: 'var(--bg-hover)' }}
        >
          {error}
        </div>
      )}

      {/* ── Plan approval card ─────────────────────────────────────── */}
      {awaitingApproval && plan && (
        <RuntimeStoryStep
          icon={<ShieldCheck size={14} />}
          title="计划确认"
          meta={`${plan.tasks.length} 个任务`}
        >
          <div
            className="rounded-md border p-2"
            style={{ borderColor: 'var(--color-warning)', background: 'var(--bg-secondary)' }}
          >
            <div className="mb-1.5 text-[11px] font-medium" style={{ color: 'var(--text-primary)' }}>
              计划已生成，等待确认后执行。
            </div>
            {plan.assumptions.length > 0 && (
              <div className="mb-1 text-[10px]" style={{ color: 'var(--text-tertiary)' }}>
                假设: {plan.assumptions.join('; ')}
              </div>
            )}
            {plan.risks.length > 0 && (
              <div className="mb-2 text-[10px]" style={{ color: 'var(--text-tertiary)' }}>
                风险: {plan.risks.join('; ')}
              </div>
            )}
            <div className="flex gap-1.5">
              <button
                onClick={() => approve(activeRun.run_id)}
                className="flex flex-1 items-center justify-center gap-1 rounded px-2 py-1 text-[11px] font-medium"
                style={{ background: 'var(--accent)', color: 'var(--text-on-accent)' }}
              >
                <Play size={12} /> 执行全部
              </button>
              <button
                onClick={() => {
                  // G3: Edit plan — user can modify tasks before approving.
                  // For now this opens the plan for inline editing via the
                  // edit_task_plan API; a full editor UI is a future enhancement.
                  const edited = window.prompt('编辑计划 (JSON 格式)', JSON.stringify(plan.tasks, null, 2));
                  if (edited) {
                    try {
                      const tasks = JSON.parse(edited);
                      import('../../api/endpoints').then(({ taskRuntimeApi }) =>
                        taskRuntimeApi.editPlan(activeRun.run_id, tasks).then(() => refresh(activeRun.run_id))
                      );
                    } catch { /* ignore parse errors */ }
                  }
                }}
                className="flex items-center justify-center gap-1 rounded px-2 py-1 text-[11px]"
                style={{ background: 'var(--bg-hover)', color: 'var(--text-secondary)' }}
              >
                编辑计划
              </button>
              <button
                onClick={() => reject(activeRun.run_id)}
                className="flex items-center justify-center gap-1 rounded px-2 py-1 text-[11px]"
                style={{ background: 'var(--bg-hover)', color: 'var(--text-secondary)' }}
              >
                <XCircle size={12} /> 取消
              </button>
            </div>
          </div>
        </RuntimeStoryStep>
      )}

      {/* ── Parallel workers (live trace) ──────────────────────────── */}
      {visibleTraceWorkers.length > 0 ? (
        <RuntimeStoryStep
          icon={<Workflow size={14} />}
          title="并行执行"
          meta={`${completedCount}/${todos.length || visibleTraceWorkers.length} 完成`}
        >
          <div className="space-y-2">
            {visibleTraceWorkers.map((worker) => (
              <WorkerTraceRow
                key={`${worker.runId}:${worker.workerId}`}
                worker={worker}
                expanded={Boolean(expandedWorkers[`${worker.runId}:${worker.workerId}`])}
                onToggle={() => {
                  const key = `${worker.runId}:${worker.workerId}`;
                  setExpandedWorkers((prev) => ({ ...prev, [key]: !prev[key] }));
                }}
                roomy
              />
            ))}
          </div>
        </RuntimeStoryStep>
      ) : runningWorkers.length > 0 && (
        <RuntimeStoryStep
          icon={<Workflow size={14} />}
          title="并行执行"
          meta={`${runningWorkers.length} 运行中`}
        >
          {runningWorkers.map((w) => (
            <div key={w.id} className="flex items-center gap-1 px-1 py-0.5 text-[10px]" style={{ color: 'var(--text-secondary)' }}>
              <Loader2 size={10} className="animate-spin" style={{ color: 'var(--color-info)' }} />
              <span className="font-mono">{w.owner_agent ?? 'worker'}</span>
              <ChevronRight size={9} style={{ color: 'var(--text-tertiary)' }} />
              <span className="truncate">{w.title}</span>
            </div>
          ))}
        </RuntimeStoryStep>
      )}

      {finalResult && (
        <RuntimeStoryStep
          icon={<CheckCircle2 size={14} />}
          title="最终任务结果"
          meta="已汇总"
        >
          <div
            className="rounded-lg border p-3"
            style={{ borderColor: 'var(--border-primary)', background: 'var(--bg-secondary)' }}
          >
            <div className="mb-2 flex justify-end">
              <TraceCopyButton
                copied={copiedFinalResult}
                onClick={() => {
                  copyToClipboard(finalResult);
                  setCopiedFinalResult(true);
                  window.setTimeout(() => setCopiedFinalResult(false), 1600);
                }}
              />
            </div>
            <ScrollableText text={finalResult} maxHeight={720} className="text-[12px]" />
          </div>
        </RuntimeStoryStep>
      )}

      {usageSummary.calls > 0 && (
        <RuntimeStoryStep
          icon={<Gauge size={14} />}
          title="Token / Cache 总结"
          meta={`${usageSummary.calls} LLM call${usageSummary.calls > 1 ? 's' : ''}`}
        >
          <CacheUsageCard summary={usageSummary} />
        </RuntimeStoryStep>
      )}

      {/* ── Artifacts ──────────────────────────────────────────────── */}
      {artifacts.length > 0 && (
        <RuntimeStoryStep
          icon={<FileText size={14} />}
          title="产出"
          meta={`${artifacts.length} 个 artifact`}
        >
          {artifacts.map((a) => (
            <div
              key={a.id}
              className="flex items-center gap-1 truncate px-1 py-0.5 text-[10px]"
              style={{ color: 'var(--text-secondary)' }}
              title={a.path ?? a.title}
            >
              <FileText size={10} style={{ color: 'var(--text-tertiary)' }} />
              <span className="truncate">{a.title}</span>
            </div>
          ))}
        </RuntimeStoryStep>
      )}

      {/* ── Files changed (G15) ────────────────────────────────────── */}
      {plan && (() => {
        const changedFiles = new Set<string>();
        plan.tasks.forEach((t) => t.files.forEach((f) => changedFiles.add(f)));
        if (changedFiles.size === 0) return null;
        return (
          <RuntimeStoryStep
            icon={<FileText size={14} />}
            title="文件变更"
            meta={`${changedFiles.size} 个文件`}
          >
            {[...changedFiles].map((f) => (
              <div key={f} className="truncate px-1 py-0.5 text-[10px]" style={{ color: 'var(--text-secondary)' }} title={f}>
                {f}
              </div>
            ))}
          </RuntimeStoryStep>
        );
      })()}

      {/* ── Review results (G15) ───────────────────────────────────── */}
      {todos.some((t) => t.status === ('failed' as TodoStatus)) && (
        <RuntimeStoryStep
          icon={<AlertCircle size={14} />}
          title="审查结果"
          meta="存在失败项"
        >
          {todos
            .filter((t) => t.status === ('failed' as TodoStatus))
            .map((t) => (
              <div key={t.id} className="px-1 py-0.5 text-[10px]" style={{ color: 'var(--color-error)' }}>
                <AlertCircle size={10} className="mr-1 inline" />
                {t.title}: {t.summary ?? '审查未通过'}
              </div>
            ))}
        </RuntimeStoryStep>
      )}

      {/* ── Test results (G15) — from verification tasks ───────────── */}
      {plan && plan.tasks.some((t) => t.kind === ('verification' as PlanTaskKind)) && (
        <RuntimeStoryStep
          icon={<CheckCircle2 size={14} />}
          title="测试 / 验证"
        >
          {todos
            .filter((t) => {
              const task = plan.tasks.find((p) => p.id === t.task_id);
              return task?.kind === ('verification' as PlanTaskKind);
            })
            .map((t) => (
              <div key={t.id} className="flex items-center gap-1 px-1 py-0.5 text-[10px]" style={{ color: 'var(--text-secondary)' }}>
                <TodoIcon status={t.status} />
                <span className="truncate">{t.title}</span>
              </div>
            ))}
        </RuntimeStoryStep>
      )}

      </div>

      {/* ── Footer actions ─────────────────────────────────────────── */}
      <div className="flex items-center justify-between">
        {/* Ready state: show an "执行" button so the user can (re)launch
            execution if auto-execute after approve failed or was skipped. */}
        {activeRun.status === 'ready' && (
          <button
            onClick={() => execute(activeRun.run_id)}
            className="flex items-center gap-1 text-[10px] font-medium"
            style={{ color: 'var(--accent)' }}
          >
            <Play size={11} /> 执行
          </button>
        )}
        {!isTerminal && activeRun.status !== 'ready' && (
          <button
            onClick={() => cancel(activeRun.run_id)}
            className="flex items-center gap-1 text-[10px]"
            style={{ color: 'var(--color-error)' }}
          >
            <XCircle size={11} /> 取消运行
          </button>
        )}
        <button
          onClick={() => refresh(activeRun.run_id)}
          className="ml-auto flex items-center gap-1 text-[10px]"
          style={{ color: 'var(--text-tertiary)' }}
        >
          <RefreshCw size={11} /> 刷新
        </button>
      </div>
    </section>
  );
}
