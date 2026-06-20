//! TaskRuntime right-rail panel.
//!
//! Renders the structured state of a complex-task run from the canonical
//! SQLite store (via taskRuntimeStore), NOT from regex-scanned chat messages.
//! Shows: run header, plan + approval actions (when AwaitingPlanApproval),
//! todo list with live status, parallel worker view, and artifacts.
//!
//! Mounted inside RightRail as a new section above "输出".

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
} from 'lucide-react';
import { useTaskRuntimeStore } from '../../stores/taskRuntimeStore';
import { useConversationStore } from '../../stores/conversationStore';
import {
  useWorkerTraceStore,
  type WorkerTraceEvent,
  type WorkerTraceState,
  type WorkerTraceStatus,
} from '../../stores/workerTraceStore';
import type { TodoStatus, PlanTaskKind } from '../../generated';

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
}: {
  worker: WorkerTraceState;
  expanded: boolean;
  onToggle: () => void;
}) {
  const latest = worker.events[worker.events.length - 1];
  const latestLabel = latest ? eventLabel(latest) : null;
  const recentEvents = expanded ? worker.events.slice(-80) : [];

  return (
    <div className="rounded px-1.5 py-1" style={{ background: 'var(--bg-secondary)' }}>
      <button
        type="button"
        onClick={onToggle}
        className="flex w-full items-start gap-1.5 text-left"
      >
        <div className="mt-0.5">
          {expanded ? (
            <ChevronDown size={12} style={{ color: 'var(--text-tertiary)' }} />
          ) : (
            <ChevronRight size={12} style={{ color: 'var(--text-tertiary)' }} />
          )}
        </div>
        <div className="mt-0.5">
          {worker.status === 'running' ? (
            <Loader2 size={12} className="animate-spin" style={{ color: 'var(--color-info)' }} />
          ) : worker.status === 'completed' ? (
            <CheckCircle2 size={12} style={{ color: 'var(--color-success)' }} />
          ) : worker.status === 'failed' ? (
            <AlertCircle size={12} style={{ color: 'var(--color-error)' }} />
          ) : (
            <Circle size={12} style={{ color: 'var(--text-tertiary)' }} />
          )}
        </div>
        <div className="min-w-0 flex-1">
          <div className="flex min-w-0 items-center gap-1">
            <span className="truncate text-[11px] font-medium" style={{ color: 'var(--text-primary)' }}>
              {worker.title ?? worker.task ?? worker.workerId}
            </span>
            <span
              className="shrink-0 rounded px-1 text-[9px]"
              style={{ color: statusColor(worker.status), background: 'var(--bg-hover)' }}
            >
              {workerStatusLabel(worker.status)}
            </span>
          </div>
          <div className="mt-0.5 flex min-w-0 items-center gap-1 text-[9px]" style={{ color: 'var(--text-tertiary)' }}>
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
        <div className="mt-1.5 border-l pl-2" style={{ borderColor: 'var(--border-primary)' }}>
          {recentEvents.map((event) => {
            const item = eventLabel(event);
            return (
              <div key={event.event_id} className="mb-1 flex gap-1.5 text-[10px] last:mb-0">
                <div className="mt-0.5 shrink-0">{item.icon}</div>
                <div className="min-w-0 flex-1">
                  <div className="flex items-center gap-1" style={{ color: 'var(--text-secondary)' }}>
                    <span className="shrink-0">{item.label}</span>
                    <span className="shrink-0 text-[9px]" style={{ color: 'var(--text-tertiary)' }}>
                      {eventTime(event.timestamp)}
                    </span>
                  </div>
                  {item.detail && (
                    <div
                      className="mt-0.5 max-h-20 overflow-hidden whitespace-pre-wrap break-words text-[9px] leading-snug"
                      style={{ color: 'var(--text-tertiary)' }}
                      title={item.detail}
                    >
                      {item.detail}
                    </div>
                  )}
                </div>
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}

export function TaskRuntimePanel() {
  const [expandedWorkers, setExpandedWorkers] = useState<Record<string, boolean>>({});
  const activeId = useConversationStore((s) => s.activeId);
  const traceWorkers = useWorkerTraceStore((s) => s.workers);
  const {
    activeRun,
    plan,
    todos,
    artifacts,
    awaitingApproval,
    error,
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

  if (!activeRun) {
    // No complex task in flight — render nothing (the section is omitted by
    // RightRail when this returns null).
    return null;
  }

  const completedCount = todos.filter((t) => t.status === ('completed' as TodoStatus)).length;
  const runningWorkers = todos.filter((t) => t.status === ('running' as TodoStatus));
  const visibleTraceWorkers = useMemo(
    () =>
      Object.values(traceWorkers)
        .filter((worker) => worker.runId === activeRun.run_id)
        .sort((a, b) => (a.startedAt ?? '').localeCompare(b.startedAt ?? '')),
    [activeRun.run_id, traceWorkers]
  );

  return (
    <section className="border-b border-[var(--border-primary)] px-3 py-2.5">
      {/* ── Run header ─────────────────────────────────────────────── */}
      <div className="mb-2 flex items-center justify-between">
        <div className="flex items-center gap-1.5">
          <ListTodo size={13} style={{ color: 'var(--accent)' }} />
          <span className="text-xs font-medium" style={{ color: 'var(--text-primary)' }}>
            任务运行
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

      {/* ── Plan task list (when there's a plan) ───────────────────── */}
      {plan && plan.tasks.length > 0 && (
        <div className="mb-2">
          <div className="mb-1 flex items-center justify-between">
            <span className="text-[10px] font-medium" style={{ color: 'var(--text-tertiary)' }}>
              计划任务
            </span>
            <span className="text-[10px]" style={{ color: 'var(--text-tertiary)' }}>
              {completedCount}/{todos.length}
            </span>
          </div>
          <div className="space-y-0.5">
            {todos.map((todo) => {
              const task = plan.tasks.find((t) => t.id === todo.task_id);
              return (
                <div
                  key={todo.id}
                  className="flex items-start gap-1.5 rounded px-1.5 py-1"
                  style={{ background: 'var(--bg-secondary)' }}
                >
                  <div className="mt-0.5">
                    <TodoIcon status={todo.status} />
                  </div>
                  <div className="min-w-0 flex-1">
                    <div
                      className="truncate text-[11px]"
                      style={{ color: 'var(--text-primary)' }}
                      title={todo.title}
                    >
                      {todo.title}
                    </div>
                    <div className="flex items-center gap-1 text-[9px]" style={{ color: 'var(--text-tertiary)' }}>
                      {task && (
                        <span
                          className="rounded px-1"
                          style={{ background: 'var(--bg-hover)' }}
                        >
                          {kindLabel(task.kind)}
                        </span>
                      )}
                      {todo.owner_agent && <span>· {todo.owner_agent}</span>}
                      <span>· {TODO_LABEL[todo.status] ?? todo.status}</span>
                    </div>
                    {todo.summary && (
                      <div className="mt-0.5 truncate text-[9px]" style={{ color: 'var(--text-tertiary)' }} title={todo.summary}>
                        {todo.summary}
                      </div>
                    )}
                  </div>
                </div>
              );
            })}
          </div>
        </div>
      )}

      {/* ── Parallel workers (live trace) ──────────────────────────── */}
      {visibleTraceWorkers.length > 0 ? (
        <div className="mb-2">
          <div className="mb-1 flex items-center justify-between">
            <span className="text-[10px] font-medium" style={{ color: 'var(--text-tertiary)' }}>
              Worker Trace
            </span>
            <span className="text-[10px]" style={{ color: 'var(--text-tertiary)' }}>
              {visibleTraceWorkers.filter((w) => w.status === 'running').length} 运行中
            </span>
          </div>
          <div className="space-y-0.5">
            {visibleTraceWorkers.map((worker) => (
              <WorkerTraceRow
                key={`${worker.runId}:${worker.workerId}`}
                worker={worker}
                expanded={Boolean(expandedWorkers[`${worker.runId}:${worker.workerId}`])}
                onToggle={() => {
                  const key = `${worker.runId}:${worker.workerId}`;
                  setExpandedWorkers((prev) => ({ ...prev, [key]: !prev[key] }));
                }}
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
