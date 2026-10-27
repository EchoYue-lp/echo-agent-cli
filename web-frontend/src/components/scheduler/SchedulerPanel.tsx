import { useEffect, useState, useCallback, useRef } from 'react';
import {
  schedulerApi,
  type SchedulerTask,
  type SchedulerManualResponse,
} from '../../api/endpoints';
import {
  Plus,
  Play,
  Trash2,
  RefreshCw,
  ToggleLeft,
  ToggleRight,
  Clock,
  X,
  CheckCircle2,
  AlertCircle,
  HelpCircle,
} from 'lucide-react';
import { useToastStore } from '../../stores/toastStore';
import { SchedulerHistory, SchedulerRunLink } from './SchedulerHistory';
import { WeixinNotificationConfiguration } from './WeixinNotification';

// ── Cron helpers ───────────────────────────────────────────────────

const CRON_FIELDS = ['分', '时', '日', '月', '周'];

function describeCron(expr: string): string {
  const parts = expr.trim().split(/\s+/);
  if (parts.length !== 5) return expr;

  const [min, hr, dom, mon, dow] = parts;

  // Every N minutes
  if (/^\*\/(\d+)$/.test(min) && hr === '*' && dom === '*' && mon === '*' && dow === '*') {
    return `每 ${min.slice(2)} 分钟`;
  }
  // Every N hours
  if (min === '0' && /^\*\/(\d+)$/.test(hr) && dom === '*' && mon === '*' && dow === '*') {
    return `每 ${hr.slice(2)} 小时`;
  }
  // Daily at HH:MM
  if (/^\d+$/.test(min) && /^\d+$/.test(hr) && dom === '*' && mon === '*' && dow === '*') {
    return `每天 ${hr.padStart(2, '0')}:${min.padStart(2, '0')}`;
  }
  // Weekly
  if (/^\d+$/.test(min) && /^\d+$/.test(hr) && dom === '*' && mon === '*' && /^\d+$/.test(dow)) {
    const days = ['周日', '周一', '周二', '周三', '周四', '周五', '周六'];
    const dayName = days[parseInt(dow, 10) % 7] || dow;
    return `每${dayName} ${hr.padStart(2, '0')}:${min.padStart(2, '0')}`;
  }
  // Monthly
  if (/^\d+$/.test(min) && /^\d+$/.test(hr) && /^\d+$/.test(dom) && mon === '*' && dow === '*') {
    return `每月 ${dom} 日 ${hr.padStart(2, '0')}:${min.padStart(2, '0')}`;
  }
  // Every minute
  if (min === '*' && hr === '*' && dom === '*' && mon === '*' && dow === '*') {
    return '每分钟';
  }

  return expr;
}

function validateCron(expr: string): string | null {
  const parts = expr.trim().split(/\s+/);
  if (parts.length !== 5) {
    return 'Cron 表达式需要 5 段：分 时 日 月 周';
  }

  const token = /^(\*|\d+|\d+-\d+|\*\/\d+|\d+(,\d+)*|\d+-\d+\/\d+)$/;
  if (!parts.every((part) => token.test(part))) {
    return 'Cron 表达式包含暂不支持的字段格式';
  }

  return null;
}

// ── Component ──────────────────────────────────────────────────────

export function SchedulerPanel() {
  const [tasks, setTasks] = useState<SchedulerTask[]>([]);
  const [loading, setLoading] = useState(true);
  const [refreshing, setRefreshing] = useState(false);
  const [showAdd, setShowAdd] = useState(false);
  const [form, setForm] = useState({ name: '', cron_expr: '', prompt: '' });
  const [editing, setEditing] = useState<SchedulerTask | null>(null);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [runResults, setRunResults] = useState<
    Record<string, Partial<SchedulerManualResponse> & { pending: boolean }>
  >({});
  const [confirmDelete, setConfirmDelete] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);
  const [busyTasks, setBusyTasks] = useState<Set<string>>(() => new Set());
  const pendingTasks = useRef(new Set<string>());
  const observer = useRef({ active: true });
  const listRequest = useRef(0);
  const addToast = useToastStore((s) => s.addToast);

  const s = {
    text: 'var(--text-primary)',
    textSec: 'var(--text-secondary)',
    textTer: 'var(--text-tertiary)',
    border: 'var(--border-primary)',
    bg: 'var(--bg-primary)',
    bgHover: 'var(--bg-hover)',
    bgCard: 'var(--bg-secondary)',
    bgInput: 'var(--bg-input)',
    accent: 'var(--accent)',
  };
  const cronError = form.cron_expr.trim() ? validateCron(form.cron_expr) : null;

  const fetchTasks = useCallback(async () => {
    const current = observer.current;
    const request = ++listRequest.current;
    setRefreshing(true);
    try {
      const data = await schedulerApi.list();
      if (current.active && request === listRequest.current) setTasks(data);
    } catch (e) {
      if (!current.active || request !== listRequest.current) return;
      console.error('Failed to fetch scheduler tasks:', e);
      addToast('error', `加载定时任务失败: ${e instanceof Error ? e.message : String(e)}`);
    } finally {
      if (current.active && request === listRequest.current) {
        setLoading(false);
        setRefreshing(false);
      }
    }
  }, [addToast]);

  useEffect(() => {
    const current = { active: true };
    observer.current = current;
    void fetchTasks();
    return () => {
      // 页面只撤销观察；原 scheduler callback 仍由后台 owner 结算。
      current.active = false;
    };
  }, [fetchTasks]);

  const beginTask = (id: string) => {
    // ref 同步拒绝同一 task 的重复点击，React 下一次 render 前也不重复准入。
    if (pendingTasks.current.has(id)) return false;
    pendingTasks.current.add(id);
    setBusyTasks((previous) => new Set(previous).add(id));
    return true;
  };
  const finishTask = (id: string, current: { active: boolean }) => {
    pendingTasks.current.delete(id);
    if (current.active)
      setBusyTasks((previous) => {
        const next = new Set(previous);
        next.delete(id);
        return next;
      });
  };

  const handleCreate = async () => {
    if (!form.name.trim() || !form.cron_expr.trim() || !form.prompt.trim()) return;
    const cronError = validateCron(form.cron_expr);
    if (cronError) {
      addToast('warning', cronError);
      return;
    }
    const current = observer.current;
    setSubmitting(true);
    setSaveError(null);
    try {
      if (editing) {
        const response = await schedulerApi.edit(editing, form);
        if (!current.active) return;
        if (!response.success) {
          setSaveError(response.error ?? response.cause ?? '修改未确认');
          addToast(
            response.outcome_unknown ? 'warning' : 'error',
            response.error ?? response.cause ?? '修改未确认'
          );
          return;
        }
      } else {
        await schedulerApi.create(form);
      }
      if (!current.active) return;
      addToast('success', editing ? '定时任务已修改' : '定时任务已创建');
      setEditing(null);
      setForm({ name: '', cron_expr: '', prompt: '' });
      setShowAdd(false);
      await fetchTasks();
    } catch (e) {
      if (!current.active) return;
      console.error('Failed to create task:', e);
      setSaveError(e instanceof Error ? e.message : String(e));
      addToast('error', `保存定时任务失败: ${e instanceof Error ? e.message : String(e)}`);
    } finally {
      if (current.active) setSubmitting(false);
    }
  };

  const handleToggle = async (task: SchedulerTask) => {
    if (!beginTask(task.id)) return;
    const current = observer.current;
    const nextEnabled = task.status !== 'enabled';
    try {
      await schedulerApi.updateStatus(task.id, nextEnabled);
      if (!current.active) return;
      addToast('success', nextEnabled ? '定时任务已启用' : '定时任务已禁用');
      await fetchTasks();
    } catch (e) {
      if (!current.active) return;
      console.error('Failed to toggle task:', e);
      addToast('error', `更新定时任务状态失败: ${e instanceof Error ? e.message : String(e)}`);
    } finally {
      finishTask(task.id, current);
    }
  };

  const handleRun = async (id: string) => {
    if (!beginTask(id)) return;
    const current = observer.current;
    setRunResults((previous) => ({ ...previous, [id]: { pending: true } }));
    try {
      const res = await schedulerApi.run(id);
      if (!current.active) return;
      setRunResults((previous) => ({ ...previous, [id]: { ...res, pending: false } }));
      if (!res.success) {
        addToast(
          res.outcome_unknown ? 'warning' : 'error',
          `手动运行未确认: ${res.error ?? res.cause}`
        );
      } else {
        addToast('success', '定时任务运行完成');
      }
      await fetchTasks();
    } catch (e) {
      if (!current.active) return;
      const message = e instanceof Error ? e.message : String(e);
      setRunResults((previous) => ({
        ...previous,
        [id]: { pending: false, outcome_unknown: true, error: message },
      }));
      addToast('warning', `手动运行回执未确认: ${message}`);
    } finally {
      finishTask(id, current);
    }
  };

  const handleDelete = async (id: string) => {
    if (!beginTask(id)) return;
    const current = observer.current;
    try {
      await schedulerApi.delete(id);
      if (!current.active) return;
      addToast('success', '定时任务已删除');
      setConfirmDelete(null);
      await fetchTasks();
    } catch (e) {
      if (!current.active) return;
      console.error('Failed to delete task:', e);
      addToast('error', `删除定时任务失败: ${e instanceof Error ? e.message : String(e)}`);
    } finally {
      finishTask(id, current);
    }
  };

  if (loading) {
    return (
      <div className="p-3">
        <p className="text-xs" style={{ color: s.textTer }}>
          加载中...
        </p>
      </div>
    );
  }

  return (
    <div className="space-y-4">
      {/* Header */}
      <div className="flex items-center justify-between">
        <h3 className="text-sm font-semibold" style={{ color: s.text }}>
          定时任务 ({tasks.length})
        </h3>
        <div className="flex gap-1">
          <button
            onClick={fetchTasks}
            disabled={refreshing}
            className="rounded-md p-1.5 transition-colors hover:opacity-80"
            style={{ color: s.textTer }}
            title="刷新"
          >
            <RefreshCw size={14} className={refreshing ? 'animate-spin' : ''} />
          </button>
          <button
            onClick={() => {
              setEditing(null);
              setSaveError(null);
              setForm({ name: '', cron_expr: '', prompt: '' });
              setShowAdd(!showAdd);
            }}
            className="rounded-md p-1.5 transition-colors hover:opacity-80"
            style={{ color: showAdd ? s.accent : s.textTer }}
            title="添加任务"
          >
            {showAdd ? <X size={14} /> : <Plus size={14} />}
          </button>
        </div>
      </div>

      {/* Definition edits carry the original captured task snapshot. */}
      {showAdd && (
        <div
          className="rounded-lg border p-4 space-y-3"
          style={{ borderColor: s.border, background: s.bgCard }}
        >
          {saveError && <p role="alert">{saveError}</p>}
          <input
            type="text"
            value={form.name}
            onChange={(e) => setForm({ ...form, name: e.target.value })}
            placeholder="任务名称"
            className="w-full rounded-lg border px-3 py-2 text-xs"
            style={{ borderColor: s.border, background: s.bgInput, color: s.text }}
          />

          <div className="space-y-1.5">
            <input
              type="text"
              value={form.cron_expr}
              onChange={(e) => setForm({ ...form, cron_expr: e.target.value })}
              placeholder="Cron 表达式，例: */5 * * * *"
              className="w-full rounded-lg border px-3 py-2 text-xs font-mono"
              style={{ borderColor: s.border, background: s.bgInput, color: s.text }}
            />
            {/* Cron field labels */}
            <div className="flex gap-0 px-1">
              {CRON_FIELDS.map((label) => (
                <span
                  key={label}
                  className="flex-1 text-center text-[10px]"
                  style={{ color: s.textTer }}
                >
                  {label}
                </span>
              ))}
            </div>
            {/* Human readable description */}
            {form.cron_expr.trim() && (
              <div
                className="flex items-center gap-1.5 rounded-md px-2 py-1 text-[11px]"
                style={{
                  background: cronError ? 'rgba(239,68,68,0.08)' : s.bgHover,
                  color: cronError ? 'var(--color-error, #ef4444)' : s.textSec,
                }}
              >
                {cronError ? <AlertCircle size={11} /> : <HelpCircle size={11} />}
                <span>{cronError ?? describeCron(form.cron_expr)}</span>
              </div>
            )}
          </div>

          <textarea
            value={form.prompt}
            onChange={(e) => setForm({ ...form, prompt: e.target.value })}
            placeholder="执行的提示词 / Prompt"
            rows={3}
            className="w-full rounded-lg border px-3 py-2 text-xs"
            style={{ borderColor: s.border, background: s.bgInput, color: s.text }}
          />

          <button
            onClick={handleCreate}
            disabled={
              submitting ||
              Boolean(cronError) ||
              !form.name.trim() ||
              !form.cron_expr.trim() ||
              !form.prompt.trim()
            }
            className="flex items-center gap-1.5 rounded-lg px-3 py-2 text-xs font-medium text-white transition-colors disabled:opacity-50"
            style={{ background: s.accent }}
          >
            <Plus size={12} /> {submitting ? '保存中...' : editing ? '保存修改' : '创建任务'}
          </button>
        </div>
      )}

      {/* Task List */}
      {tasks.length === 0 ? (
        <p className="py-6 text-center text-xs" style={{ color: s.textTer }}>
          暂无定时任务，点击右上角 + 添加
        </p>
      ) : (
        <div className="space-y-2">
          {tasks.map((task) => {
            const enabled = task.status === 'enabled';
            const runResult = runResults[task.id];
            return (
              <div
                key={task.id}
                className="rounded-lg border px-4 py-3 space-y-2"
                style={{ borderColor: s.border, background: s.bg }}
              >
                {/* Row 1: name + status badge + toggle */}
                <div className="flex items-center gap-2">
                  <span className="text-xs font-semibold flex-1 truncate" style={{ color: s.text }}>
                    {task.name}
                  </span>
                  <span
                    className="rounded-full px-2 py-0.5 text-[10px] font-medium"
                    style={{
                      background: enabled ? 'rgba(34,197,94,0.12)' : 'rgba(156,163,175,0.15)',
                      color: enabled ? 'var(--color-success, #22c55e)' : s.textTer,
                    }}
                  >
                    {enabled ? '启用' : '禁用'}
                  </span>
                  <button
                    onClick={() => handleToggle(task)}
                    disabled={busyTasks.has(task.id)}
                    className="transition-colors"
                    style={{ color: enabled ? 'var(--color-success, #22c55e)' : s.textTer }}
                    title={enabled ? '禁用' : '启用'}
                  >
                    {enabled ? <ToggleRight size={16} /> : <ToggleLeft size={16} />}
                  </button>
                </div>

                {/* Row 2: cron + description */}
                <div className="flex items-center gap-2 text-[11px]" style={{ color: s.textSec }}>
                  <code
                    className="rounded-md px-1.5 py-0.5 font-mono"
                    style={{ background: s.bgHover, color: s.text }}
                  >
                    {task.cron_expr}
                  </code>
                  <span style={{ color: s.textTer }}>→</span>
                  <span>{describeCron(task.cron_expr)}</span>
                </div>

                {/* Row 3: ID + times */}
                <div
                  className="flex flex-wrap items-center gap-x-4 gap-y-1 text-[10px]"
                  style={{ color: s.textTer }}
                >
                  <span>ID: {task.id.slice(0, 8)}</span>
                  {task.last_run_at && (
                    <span className="flex items-center gap-1">
                      <Clock size={10} /> 上次: {new Date(task.last_run_at).toLocaleString()}
                    </span>
                  )}
                  {task.next_run && enabled && (
                    <span className="flex items-center gap-1">
                      <Clock size={10} /> 下次: {new Date(task.next_run).toLocaleString()}
                    </span>
                  )}
                </div>

                {/* Row 4: prompt preview */}
                <p
                  className="text-[11px] line-clamp-2"
                  style={{ color: s.textSec }}
                  title={task.prompt}
                >
                  {task.prompt}
                </p>

                {/* Row 5: actions */}
                <div className="flex items-center gap-2 pt-1">
                  <button
                    aria-label={`编辑${task.name}`}
                    onClick={() => {
                      setEditing(task);
                      setForm({ name: task.name, cron_expr: task.cron_expr, prompt: task.prompt });
                      setSaveError(null);
                      setShowAdd(true);
                    }}
                  >
                    编辑
                  </button>
                  <button
                    onClick={() => handleRun(task.id)}
                    disabled={busyTasks.has(task.id)}
                    className="flex items-center gap-1 rounded-md px-2 py-1 text-[11px] font-medium transition-colors"
                    style={{
                      background: 'var(--action-run-bg)',
                      color: 'var(--action-run)',
                    }}
                  >
                    {busyTasks.has(task.id) && runResult?.pending ? (
                      <RefreshCw size={11} className="animate-spin" />
                    ) : (
                      <Play size={11} />
                    )}{' '}
                    手动运行
                  </button>
                  {confirmDelete === task.id ? (
                    <div className="flex items-center gap-1">
                      <span
                        className="text-[10px]"
                        style={{ color: 'var(--color-error, #ef4444)' }}
                      >
                        确认删除?
                      </span>
                      <button
                        onClick={() => handleDelete(task.id)}
                        disabled={busyTasks.has(task.id)}
                        className="rounded-md p-1 transition-colors"
                        style={{ color: 'var(--color-error, #ef4444)' }}
                      >
                        <CheckCircle2 size={12} />
                      </button>
                      <button
                        onClick={() => setConfirmDelete(null)}
                        className="rounded-md p-1 transition-colors"
                        style={{ color: s.textTer }}
                      >
                        <X size={12} />
                      </button>
                    </div>
                  ) : (
                    <button
                      onClick={() => setConfirmDelete(task.id)}
                      disabled={busyTasks.has(task.id)}
                      className="flex items-center gap-1 rounded-md px-2 py-1 text-[11px] transition-colors"
                      style={{ color: 'var(--color-error, #ef4444)' }}
                    >
                      <Trash2 size={11} /> 删除
                    </button>
                  )}
                </div>

                <WeixinNotificationConfiguration task={task} />

                {/* Run result */}
                {runResult && (
                  <div
                    className="mt-1 rounded-lg border p-2 text-[11px]"
                    style={{
                      borderColor: runResult.error ? 'rgba(239,68,68,0.3)' : s.border,
                      background: runResult.error ? 'rgba(239,68,68,0.05)' : s.bgCard,
                    }}
                  >
                    {runResult.pending && (
                      <div className="flex items-center gap-1.5" style={{ color: s.textSec }}>
                        <RefreshCw size={11} className="animate-spin" /> 运行中...
                      </div>
                    )}
                    {runResult.outcome_unknown && (
                      <p role="alert">结算未知；请读取原运行历史，勿直接重试。</p>
                    )}
                    {runResult.occurrence && (
                      <p>
                        执行：{runResult.occurrence.occurrence_id} · 定义{' '}
                        {runResult.occurrence.definition_id} · 尝试 {runResult.attempt_id}
                      </p>
                    )}
                    {runResult.run_lookup?.run && (
                      <SchedulerRunLink run={runResult.run_lookup.run} />
                    )}
                    {!runResult.pending && !runResult.run_lookup?.run && (
                      <p>尚无可定位的 TaskRun</p>
                    )}
                    {runResult.error && (
                      <div
                        className="flex items-start gap-1.5"
                        style={{ color: 'var(--color-error, #ef4444)' }}
                      >
                        <AlertCircle size={11} className="mt-0.5 flex-shrink-0" />
                        <pre className="whitespace-pre-wrap">{runResult.error}</pre>
                      </div>
                    )}
                    {runResult.result != null && (
                      <div className="space-y-1">
                        <div
                          className="flex items-center gap-1 font-medium"
                          style={{
                            color: runResult.success ? 'var(--color-success, #22c55e)' : s.textSec,
                          }}
                        >
                          <CheckCircle2 size={11} />{' '}
                          {runResult.success ? '运行完成' : '已返回执行内容，结算未确认'}
                        </div>
                        <pre
                          className="whitespace-pre-wrap text-[10px]"
                          style={{ color: s.textSec }}
                        >
                          {runResult.result.slice(0, 1000)}
                          {(runResult.result?.length ?? 0) > 1000 && '...'}
                        </pre>
                      </div>
                    )}
                  </div>
                )}

                {/* Last run result from DB */}
                {task.last_result && !runResult && (
                  <details className="text-[11px]">
                    <summary className="cursor-pointer" style={{ color: s.textTer }}>
                      上次运行结果
                    </summary>
                    <pre
                      className="mt-1 whitespace-pre-wrap rounded-md p-2 text-[10px]"
                      style={{ background: s.bgCard, color: s.textSec }}
                    >
                      {task.last_result.slice(0, 500)}
                    </pre>
                  </details>
                )}
              </div>
            );
          })}
        </div>
      )}
      <SchedulerHistory />
    </div>
  );
}
