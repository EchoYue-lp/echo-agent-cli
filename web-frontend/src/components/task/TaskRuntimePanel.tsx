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
} from 'lucide-react';
import { useTaskRuntimeStore } from '../../stores/taskRuntimeStore';
import { useConversationStore } from '../../stores/conversationStore';
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

  const runId = activeRun?.run_id ?? routeExplanation?.runId;
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
  const activeId = useConversationStore((s) => s.activeId);
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

      {effectiveRouteExplanation && (
        <div className="mb-3 flex gap-2 rounded-lg p-3" style={{ background: 'var(--bg-secondary)' }}>
          <Sparkles size={15} className="mt-0.5 shrink-0" style={{ color: 'var(--accent)' }} />
          <div className="min-w-0 flex-1">
            <div className="mb-1 text-[12px] font-medium" style={{ color: 'var(--text-primary)' }}>
              已进入 TaskRuntime，并发委派 {routeWorkerCount(effectiveRouteExplanation, visibleTraceWorkers.length)} 个 worker
            </div>
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
          </div>
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

      {/* ── Plan approval card ─────────────────────────────────────── */}
      {awaitingApproval && plan && (
        <div
          className="mb-2 rounded-md border p-2"
          style={{ borderColor: 'var(--color-warning)', background: 'var(--bg-secondary)' }}
        >
          <div className="mb-1.5 text-[11px] font-medium" style={{ color: 'var(--text-primary)' }}>
            计划已生成 · {plan.tasks.length} 个任务
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
      )}

      {/* ── Parallel workers (live trace) ──────────────────────────── */}
      {visibleTraceWorkers.length > 0 ? (
        <div className="mb-3">
          <div className="mb-1 flex items-center justify-between">
            <span className="text-[12px] font-medium" style={{ color: 'var(--text-primary)' }}>
              Subagent / Worker Trace
            </span>
            <span className="text-[11px]" style={{ color: 'var(--text-tertiary)' }}>
              {completedCount}/{todos.length || visibleTraceWorkers.length} 完成
            </span>
          </div>
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
        </div>
      ) : runningWorkers.length > 0 && (
        <div className="mb-2">
          <div className="mb-1 text-[10px] font-medium" style={{ color: 'var(--text-tertiary)' }}>
            并行执行 {runningWorkers.length}
          </div>
          {runningWorkers.map((w) => (
            <div key={w.id} className="flex items-center gap-1 px-1 py-0.5 text-[10px]" style={{ color: 'var(--text-secondary)' }}>
              <Loader2 size={10} className="animate-spin" style={{ color: 'var(--color-info)' }} />
              <span className="font-mono">{w.owner_agent ?? 'worker'}</span>
              <ChevronRight size={9} style={{ color: 'var(--text-tertiary)' }} />
              <span className="truncate">{w.title}</span>
            </div>
          ))}
        </div>
      )}

      {finalResult && (
        <div
          className="mb-3 rounded-lg border p-3"
          style={{ borderColor: 'var(--border-primary)', background: 'var(--bg-secondary)' }}
        >
          <div className="mb-2 flex items-center justify-between gap-2">
            <div className="flex items-center gap-1.5 text-[12px] font-medium" style={{ color: 'var(--text-primary)' }}>
              <CheckCircle2 size={14} style={{ color: 'var(--color-success)' }} />
              <span>最终任务结果</span>
            </div>
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
      )}

      {/* ── Artifacts ──────────────────────────────────────────────── */}
      {artifacts.length > 0 && (
        <div className="mb-2">
          <div className="mb-1 flex items-center gap-1 text-[10px] font-medium" style={{ color: 'var(--text-tertiary)' }}>
            <FileText size={10} /> 产出 ({artifacts.length})
          </div>
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
        </div>
      )}

      {/* ── Files changed (G15) ────────────────────────────────────── */}
      {plan && (() => {
        const changedFiles = new Set<string>();
        plan.tasks.forEach((t) => t.files.forEach((f) => changedFiles.add(f)));
        if (changedFiles.size === 0) return null;
        return (
          <div className="mb-2">
            <div className="mb-1 text-[10px] font-medium" style={{ color: 'var(--text-tertiary)' }}>
              文件变更 ({changedFiles.size})
            </div>
            {[...changedFiles].map((f) => (
              <div key={f} className="truncate px-1 py-0.5 text-[10px]" style={{ color: 'var(--text-secondary)' }} title={f}>
                {f}
              </div>
            ))}
          </div>
        );
      })()}

      {/* ── Review results (G15) ───────────────────────────────────── */}
      {todos.some((t) => t.status === ('failed' as TodoStatus)) && (
        <div className="mb-2">
          <div className="mb-1 text-[10px] font-medium" style={{ color: 'var(--color-warning)' }}>
            审查结果
          </div>
          {todos
            .filter((t) => t.status === ('failed' as TodoStatus))
            .map((t) => (
              <div key={t.id} className="px-1 py-0.5 text-[10px]" style={{ color: 'var(--color-error)' }}>
                <AlertCircle size={10} className="mr-1 inline" />
                {t.title}: {t.summary ?? '审查未通过'}
              </div>
            ))}
        </div>
      )}

      {/* ── Test results (G15) — from verification tasks ───────────── */}
      {plan && plan.tasks.some((t) => t.kind === ('verification' as PlanTaskKind)) && (
        <div className="mb-2">
          <div className="mb-1 text-[10px] font-medium" style={{ color: 'var(--text-tertiary)' }}>
            测试 / 验证
          </div>
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
        </div>
      )}

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
