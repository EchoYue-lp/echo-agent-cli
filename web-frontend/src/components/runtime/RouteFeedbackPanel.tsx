import { useEffect, useMemo, useState } from 'react';
import { BrainCircuit, Plus, RefreshCw, Trash2 } from 'lucide-react';
import { taskRuntimeApi, type RouteFeedbackRule } from '../../api/endpoints';

const ROUTES = [
  { id: 'normal_chat', label: 'Chat' },
  { id: 'plan_only', label: 'Plan' },
  { id: 'complex_runtime', label: 'TaskRuntime' },
  { id: 'parallel_readonly_delegation', label: '只读并行' },
] as const;

const COMMON_WORKERS = [
  'project_explorer',
  'code_reviewer',
  'test_planner',
  'literature_scout',
  'evidence_reviewer',
  'data_profiler',
  'analysis_reviewer',
  'safety_reviewer',
  'summary_writer',
];

function routeLabel(route: string): string {
  return ROUTES.find((item) => item.id === route)?.label ?? route;
}

function splitWorkers(value: string): string[] {
  return value
    .split(',')
    .map((item) => item.trim())
    .filter(Boolean);
}

export function RouteFeedbackPanel() {
  const [rules, setRules] = useState<RouteFeedbackRule[]>([]);
  const [pattern, setPattern] = useState('');
  const [route, setRoute] = useState<(typeof ROUTES)[number]['id']>('normal_chat');
  const [reason, setReason] = useState('');
  const [workers, setWorkers] = useState('');
  const [loading, setLoading] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const load = async () => {
    setLoading(true);
    setError(null);
    try {
      setRules(await taskRuntimeApi.listRouteFeedbackRules());
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    void load();
  }, []);

  const selectedRouteNeedsWorkers = route === 'parallel_readonly_delegation' || route === 'complex_runtime';
  const canSave = pattern.trim().length > 0 && !saving;

  const sortedRules = useMemo(
    () => [...rules].sort((a, b) => a.pattern.localeCompare(b.pattern)),
    [rules]
  );

  const save = async () => {
    if (!canSave) return;
    setSaving(true);
    setError(null);
    try {
      const next = await taskRuntimeApi.upsertRouteFeedbackRule(
        pattern,
        route,
        reason.trim() || 'user route correction',
        splitWorkers(workers)
      );
      setRules(next);
      setPattern('');
      setReason('');
      setWorkers('');
      setRoute('normal_chat');
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSaving(false);
    }
  };

  const remove = async (rule: RouteFeedbackRule) => {
    setError(null);
    try {
      setRules(await taskRuntimeApi.deleteRouteFeedbackRule(rule.pattern));
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  };

  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="mb-4 flex items-center justify-between gap-3">
        <div>
          <h2 className="text-sm font-semibold" style={{ color: 'var(--text-primary)' }}>
            路由学习
          </h2>
          <p className="mt-1 text-xs" style={{ color: 'var(--text-tertiary)' }}>
            Auto 模式会在 LLM 路由和确定性信号之后应用这些纠偏规则。
          </p>
        </div>
        <button
          onClick={() => void load()}
          className="flex items-center gap-1 rounded-md px-2 py-1 text-xs"
          style={{ background: 'var(--bg-hover)', color: 'var(--text-secondary)' }}
        >
          <RefreshCw size={13} className={loading ? 'animate-spin' : ''} /> 刷新
        </button>
      </div>

      {error && (
        <div
          className="mb-3 rounded-md px-3 py-2 text-xs"
          style={{ background: 'var(--bg-hover)', color: 'var(--color-error)' }}
        >
          {error}
        </div>
      )}

      <section
        className="mb-4 rounded-lg border p-3"
        style={{ borderColor: 'var(--border-secondary)', background: 'var(--bg-secondary)' }}
      >
        <div className="mb-3 flex items-center gap-2 text-xs font-semibold" style={{ color: 'var(--text-primary)' }}>
          <BrainCircuit size={14} /> 新增纠偏
        </div>
        <div className="grid grid-cols-[1fr_160px] gap-2 max-sm:grid-cols-1">
          <input
            value={pattern}
            onChange={(event) => setPattern(event.target.value)}
            placeholder="匹配短语或完整请求"
            className="h-9 rounded-md border px-3 text-sm outline-none"
            style={{
              borderColor: 'var(--border-secondary)',
              background: 'var(--bg-primary)',
              color: 'var(--text-primary)',
            }}
          />
          <select
            value={route}
            onChange={(event) => setRoute(event.target.value as (typeof ROUTES)[number]['id'])}
            className="h-9 rounded-md border px-2 text-sm outline-none"
            style={{
              borderColor: 'var(--border-secondary)',
              background: 'var(--bg-primary)',
              color: 'var(--text-primary)',
            }}
          >
            {ROUTES.map((item) => (
              <option key={item.id} value={item.id}>
                {item.label}
              </option>
            ))}
          </select>
        </div>
        <textarea
          value={reason}
          onChange={(event) => setReason(event.target.value)}
          placeholder="原因"
          rows={2}
          className="mt-2 w-full resize-none rounded-md border px-3 py-2 text-sm outline-none"
          style={{
            borderColor: 'var(--border-secondary)',
            background: 'var(--bg-primary)',
            color: 'var(--text-primary)',
          }}
        />
        {selectedRouteNeedsWorkers && (
          <div className="mt-2">
            <input
              value={workers}
              onChange={(event) => setWorkers(event.target.value)}
              placeholder="worker，用英文逗号分隔"
              className="h-9 w-full rounded-md border px-3 text-sm outline-none"
              style={{
                borderColor: 'var(--border-secondary)',
                background: 'var(--bg-primary)',
                color: 'var(--text-primary)',
              }}
            />
            <div className="mt-2 flex flex-wrap gap-1">
              {COMMON_WORKERS.map((worker) => (
                <button
                  key={worker}
                  onClick={() => {
                    const current = splitWorkers(workers);
                    if (!current.includes(worker)) setWorkers([...current, worker].join(', '));
                  }}
                  className="rounded px-2 py-1 text-[11px]"
                  style={{ background: 'var(--bg-hover)', color: 'var(--text-tertiary)' }}
                >
                  {worker}
                </button>
              ))}
            </div>
          </div>
        )}
        <button
          onClick={() => void save()}
          disabled={!canSave}
          className="mt-3 flex h-8 items-center gap-1 rounded-md px-3 text-xs font-medium disabled:opacity-50"
          style={{ background: 'var(--accent)', color: 'var(--text-on-accent)' }}
        >
          <Plus size={13} /> {saving ? '保存中' : '保存'}
        </button>
      </section>

      <section className="min-h-0 flex-1 overflow-auto">
        <div className="mb-2 flex items-center justify-between">
          <h3 className="text-xs font-semibold" style={{ color: 'var(--text-primary)' }}>
            已学习规则
          </h3>
          <span className="text-xs" style={{ color: 'var(--text-tertiary)' }}>
            {rules.length}
          </span>
        </div>
        <div className="space-y-2">
          {sortedRules.map((rule) => (
            <div
              key={`${rule.pattern}:${rule.route}`}
              className="rounded-lg border p-3"
              style={{ borderColor: 'var(--border-secondary)', background: 'var(--bg-secondary)' }}
            >
              <div className="flex items-start justify-between gap-3">
                <div className="min-w-0">
                  <div className="break-words text-sm font-medium" style={{ color: 'var(--text-primary)' }}>
                    {rule.pattern}
                  </div>
                  <div className="mt-1 flex flex-wrap gap-1">
                    <span className="rounded px-2 py-0.5 text-[11px]" style={{ background: 'var(--bg-hover)', color: 'var(--text-secondary)' }}>
                      {routeLabel(rule.route)}
                    </span>
                    {rule.suggested_workers.map((worker) => (
                      <span key={worker} className="rounded px-2 py-0.5 text-[11px]" style={{ background: 'var(--bg-hover)', color: 'var(--text-tertiary)' }}>
                        {worker}
                      </span>
                    ))}
                  </div>
                </div>
                <button
                  onClick={() => void remove(rule)}
                  className="flex h-7 w-7 shrink-0 items-center justify-center rounded-md"
                  style={{ color: 'var(--text-tertiary)' }}
                  title="删除"
                >
                  <Trash2 size={14} />
                </button>
              </div>
              {rule.reason && (
                <div className="mt-2 whitespace-pre-wrap break-words text-xs" style={{ color: 'var(--text-tertiary)' }}>
                  {rule.reason}
                </div>
              )}
            </div>
          ))}
          {sortedRules.length === 0 && (
            <div
              className="rounded-lg border border-dashed p-6 text-center text-xs"
              style={{ borderColor: 'var(--border-secondary)', color: 'var(--text-tertiary)' }}
            >
              暂无路由纠偏规则
            </div>
          )}
        </div>
      </section>
    </div>
  );
}
