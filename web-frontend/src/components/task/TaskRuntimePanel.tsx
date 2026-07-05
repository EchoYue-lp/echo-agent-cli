//! TaskRuntime panels.
//!
//! Renders the structured state of a complex-task run from the canonical
//! SQLite store (via taskRuntimeStore), NOT from regex-scanned chat messages.
//! Shows: run header, plan + approval actions (when AwaitingPlanApproval),
//! todo list with live status, and artifacts.
//!
//! The compact panel is mounted inside RightRail; the full detail panel is
//! mounted in the main chat/work area.

import { useEffect, useMemo } from 'react';
import {
  CheckCircle2,
  Circle,
  Loader2,
  AlertCircle,
  ListTodo,
  RefreshCw,
  Gauge,
  Pencil,
  Trash2,
  Plus,
  Play,
  Edit3,
  X,
  AlertTriangle,
} from 'lucide-react';
import { useTaskRuntimeStore } from '../../stores/taskRuntimeStore';
import { useConversationStore } from '../../stores/conversationStore';
import {
  useSubagentRunStore,
  type ExecutionEvent,
  type SubagentRunState,
} from '../../stores/subagentRunStore';
import type { TodoStatus } from '../../generated';

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
  skipped: '已跳过',
};

function statusColor(status: string): string {
  if (['completed'].includes(status)) return 'var(--color-success)';
  if (['running'].includes(status)) return 'var(--color-info)';
  if (['failed', 'cancelled'].includes(status)) return 'var(--color-error)';
  if (['paused', 'blocked'].includes(status)) return 'var(--color-warning)';
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
  const usageEvents = events.filter((e) => e.event === 'usage');
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
  return cacheUsageFromEvents(runs.flatMap((run) => run.events));
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
    <div
      className={`rounded-lg border ${compact ? 'p-2' : 'p-3'}`}
      style={{ borderColor: 'var(--border-primary)', background: 'var(--bg-secondary)' }}
    >
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

function traceRunForTodo(todo: { task_id: string }, runs: SubagentRunState[]) {
  return runs
    .filter((run) => run.taskId === todo.task_id)
    .sort((a, b) => b.startedAt - a.startedAt)[0];
}

function displayedTodoStatus(
  todo: { status: TodoStatus; task_id: string },
  runs: SubagentRunState[]
): TodoStatus {
  const run = traceRunForTodo(todo, runs);
  if (!run) return todo.status;
  if (run.status === 'completed' && todo.status !== ('completed' as TodoStatus)) {
    return 'completed' as TodoStatus;
  }
  if (run.status === 'failed' && todo.status !== ('failed' as TodoStatus)) {
    return 'failed' as TodoStatus;
  }
  if (run.status === 'cancelled' && todo.status !== ('skipped' as TodoStatus)) {
    return 'skipped' as TodoStatus;
  }
  return todo.status;
}

export function TaskRuntimePanel() {
  const activeId = useConversationStore((s) => s.activeId);
  const traceRuns = useSubagentRunStore((s) => s.runs);
  const { activeRun, plan, todos, routeExplanation, loadByConversation, refresh, resumeTaskRun } =
    useTaskRuntimeStore();

  useEffect(() => {
    if (activeId) loadByConversation(activeId);
  }, [activeId, loadByConversation]);

  const visibleTraceRuns = useMemo(
    () =>
      activeRun
        ? Object.values(traceRuns)
            // P1.0: each inline worker now has its own run_id (independent of
            // the chat turn's root_message_id). Show ALL worker runs that belong
            // to this conversation, not just the single activeRun. Falls back to
            // exact run_id match for non-inline (DAG) runs that don't carry a
            // conversationId.
            .filter(
              (run) =>
                run.runId === activeRun.run_id ||
                run.conversationId === activeRun.conversation_id
            )
            .sort((a, b) => a.startedAt - b.startedAt)
        : [],
    [activeRun, traceRuns]
  );

  if (!activeRun && !routeExplanation) return null;

  // Detailed execution timeline is now in ConversationTimeline (main panel).
  // This right-rail panel serves as a compact status summary only.
  const runId = activeRun?.run_id ?? routeExplanation?.runId;
  const usageSummary = cacheUsageForRuns(visibleTraceRuns);
  const completedCount = todos.filter(
    (t) => displayedTodoStatus(t, visibleTraceRuns) === ('completed' as TodoStatus)
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
          {activeRun ? (STATUS_LABEL[activeRun.status] ?? activeRun.status) : '连接中'}
        </span>
      </div>

      <div
        className="mb-2 truncate rounded-md px-2 py-1.5 text-[11px]"
        style={{ background: 'var(--bg-secondary)', color: 'var(--text-secondary)' }}
        title={activeRun?.goal ?? routeExplanation?.goal}
      >
        {activeRun?.goal ?? routeExplanation?.goal ?? '正在读取任务'}
      </div>

      {activeRun?.status === 'paused' && routeExplanation?.route === 'complex_runtime' && (
        <div className="mb-2">
          <button
            onClick={() => resumeTaskRun()}
            className="w-full rounded px-3 py-1.5 text-[11px] font-medium"
            style={{ background: 'var(--accent)', color: 'var(--text-on-accent)' }}
          >
            开始执行
          </button>
        </div>
      )}

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
              const status = displayedTodoStatus(todo, visibleTraceRuns);
              const isEditable = status === 'pending' || status === 'blocked';
              return (
                <div
                  key={todo.id}
                  className="group/task flex items-start gap-1.5 rounded px-1.5 py-1 hover:bg-[var(--bg-hover)]"
                  style={{ background: 'var(--bg-secondary)' }}
                >
                  <div className="mt-0.5">
                    <TodoIcon status={status} />
                  </div>
                  <div className="min-w-0 flex-1">
                    <div
                      className="truncate text-[11px]"
                      style={{ color: 'var(--text-primary)' }}
                      title={todo.title}
                    >
                      {todo.title}
                    </div>
                    <div
                      className="flex min-w-0 items-center gap-1 text-[9px]"
                      style={{ color: 'var(--text-tertiary)' }}
                    >
                      {task && (
                        <span className="rounded px-1" style={{ background: 'var(--bg-hover)' }}>
                          {kindLabel(task.kind)}
                        </span>
                      )}
                      {todo.owner_agent && <span className="truncate">· {todo.owner_agent}</span>}
                      <span>· {TODO_LABEL[status] ?? status}</span>
                    </div>
                  </div>
                  {/* Edit/delete buttons — visible on hover, only for editable tasks */}
                  {isEditable && (
                    <div className="flex gap-0.5">
                      <button
                        className="rounded p-0.5 hover:bg-[var(--bg-active)]"
                        title="编辑"
                        onClick={() => {
                          const newTitle = prompt('新标题', todo.title);
                          if (newTitle && newTitle !== todo.title) {
                            useTaskRuntimeStore
                              .getState()
                              .updateTask(todo.task_id, { title: newTitle });
                          }
                        }}
                      >
                        <Pencil size={10} style={{ color: 'var(--text-tertiary)' }} />
                      </button>
                      <button
                        className="rounded p-0.5 hover:bg-[var(--bg-active)]"
                        title="删除"
                        onClick={() => {
                          if (confirm(`确定删除任务「${todo.title}」？`)) {
                            useTaskRuntimeStore.getState().removeTask(todo.task_id);
                          }
                        }}
                      >
                        <Trash2 size={10} style={{ color: 'var(--color-error)' }} />
                      </button>
                    </div>
                  )}
                </div>
              );
            })}
          </div>
          {/* Add task button */}
          <button
            className="mt-1 flex w-full items-center justify-center gap-1 rounded px-2 py-1 text-[10px] hover:bg-[var(--bg-hover)]"
            style={{ color: 'var(--text-tertiary)', border: '1px dashed var(--border-secondary)' }}
            onClick={() => {
              const title = prompt('新任务标题');
              if (title) {
                const lastId = todos.length > 0 ? todos[todos.length - 1].task_id : null;
                useTaskRuntimeStore.getState().insertTask(lastId, {
                  id: `task_${Date.now()}`,
                  title,
                  description: '',
                  kind: 'implementation',
                  agent_role: 'general',
                  domain_profile: 'general',
                  depends_on: [],
                  files: [],
                  allowed_tools: [],
                  verification: [],
                  retry_count: 0,
                  max_retries: 3,
                  status: 'pending',
                });
              }
            }}
          >
            <Plus size={10} /> 新增任务
          </button>
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

/**
 * Interrupt prompt dialog — shown when a new message arrives while a run is
 * in-progress. Lets the user choose: resume, edit-and-resume, or abandon.
 */
export function InterruptPromptDialog() {
  const interruptPrompt = useTaskRuntimeStore((s) => s.interruptPrompt);
  const dismiss = useTaskRuntimeStore((s) => s.dismissInterruptPrompt);
  const resume = useTaskRuntimeStore((s) => s.resumeTaskRun);

  if (!interruptPrompt) return null;

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center"
      style={{ background: 'rgba(0,0,0,0.4)' }}
    >
      <div
        className="mx-4 w-full max-w-sm rounded-lg p-4 shadow-lg"
        style={{ background: 'var(--bg-primary)', border: '1px solid var(--border-primary)' }}
      >
        <div className="mb-3 flex items-center justify-between">
          <span className="text-sm font-medium" style={{ color: 'var(--text-primary)' }}>
            任务正在执行中
          </span>
          <button onClick={dismiss} className="rounded p-1 hover:bg-[var(--bg-hover)]">
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
            className="flex items-center gap-2 rounded px-3 py-2 text-xs hover:bg-[var(--bg-hover)]"
            style={{ border: '1px solid var(--border-primary)', color: 'var(--text-primary)' }}
            onClick={() => {
              resume();
            }}
          >
            <Play size={12} /> 继续执行旧计划
          </button>
          <button
            className="flex items-center gap-2 rounded px-3 py-2 text-xs hover:bg-[var(--bg-hover)]"
            style={{ border: '1px solid var(--border-primary)', color: 'var(--text-primary)' }}
            onClick={async () => {
              // Just dismiss the prompt — the user will be able to edit and re-run.
              dismiss();
            }}
          >
            <Edit3 size={12} /> 编辑计划后继续
          </button>
          <button
            className="flex items-center gap-2 rounded px-3 py-2 text-xs hover:bg-[var(--bg-hover)]"
            style={{ border: '1px solid var(--border-primary)', color: 'var(--color-error)' }}
            onClick={async () => {
              const runId = interruptPrompt.runId;
              try {
                const { invoke } = await import('@tauri-apps/api/core');
                await invoke('cancel_task_run', { runId });
              } catch {
                // ignore
              }
              dismiss();
            }}
          >
            <Trash2 size={12} /> 废弃旧计划，开始新任务
          </button>
        </div>
      </div>
    </div>
  );
}
