import { useCallback, useEffect, useMemo, useState } from 'react';
import {
  AlertTriangle,
  Braces,
  CheckCircle2,
  ChevronRight,
  CircleGauge,
  Clock3,
  Database,
  RefreshCw,
  ShieldCheck,
  Sparkles,
  XCircle,
} from 'lucide-react';
import {
  runDiagnosticsApi,
  type DiagnosticRunSummary,
  type LlmContextBreakdown,
  type RunDiagnostics,
} from '../../api/endpoints';

const CONTEXT_PARTS: Array<{
  key: keyof LlmContextBreakdown;
  label: string;
  color: string;
}> = [
  { key: 'system_tokens', label: 'System', color: '#22c55e' },
  { key: 'user_tokens', label: 'User', color: '#3b82f6' },
  { key: 'assistant_tokens', label: 'Assistant', color: '#8b5cf6' },
  { key: 'tool_tokens', label: 'Tools', color: '#f59e0b' },
  { key: 'summary_tokens', label: 'Summary', color: '#ec4899' },
  { key: 'memory_tokens', label: 'Memory', color: '#14b8a6' },
];

function formatNumber(value?: number | null): string {
  if (value == null) return 'unknown';
  return new Intl.NumberFormat().format(value);
}

function formatPercent(value?: number | null): string {
  return value == null ? 'unknown' : `${(value * 100).toFixed(1)}%`;
}

function formatTime(value: string): string {
  const parsed = new Date(value);
  return Number.isNaN(parsed.getTime()) ? value : parsed.toLocaleString();
}

function shortId(value: string): string {
  return value.length > 12 ? `${value.slice(0, 12)}...` : value;
}

function statusIcon(status: string) {
  if (status === 'completed') return <CheckCircle2 size={14} className="text-emerald-500" />;
  if (status === 'failed' || status === 'cancelled') {
    return <XCircle size={14} className="text-red-500" />;
  }
  return <Clock3 size={14} className="text-amber-500" />;
}

function Stat({ label, value }: { label: string; value: string }) {
  return (
    <div
      className="min-w-0 rounded-md border px-3 py-2"
      style={{ borderColor: 'var(--border-subtle)', background: 'var(--bg-secondary)' }}
    >
      <div className="text-[11px]" style={{ color: 'var(--text-tertiary)' }}>
        {label}
      </div>
      <div
        className="mt-1 truncate font-mono text-sm font-semibold"
        style={{ color: 'var(--text-primary)' }}
        title={value}
      >
        {value}
      </div>
    </div>
  );
}

export function ObservabilityPanel() {
  const [runs, setRuns] = useState<DiagnosticRunSummary[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [diagnostics, setDiagnostics] = useState<RunDiagnostics | null>(null);
  const [loadingRuns, setLoadingRuns] = useState(false);
  const [loadingDetails, setLoadingDetails] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const loadRuns = useCallback(async () => {
    setLoadingRuns(true);
    setError(null);
    try {
      const nextRuns = await runDiagnosticsApi.list();
      setRuns(nextRuns);
      setSelectedId((current) => {
        if (current && nextRuns.some((run) => run.diagnostic_id === current)) return current;
        return nextRuns[0]?.diagnostic_id ?? null;
      });
    } catch (loadError) {
      setError(loadError instanceof Error ? loadError.message : String(loadError));
    } finally {
      setLoadingRuns(false);
    }
  }, []);

  useEffect(() => {
    void loadRuns();
  }, [loadRuns]);

  useEffect(() => {
    if (!selectedId) {
      setDiagnostics(null);
      return;
    }
    let active = true;
    setLoadingDetails(true);
    setError(null);
    void runDiagnosticsApi
      .get(selectedId)
      .then((value) => {
        if (active) setDiagnostics(value);
      })
      .catch((loadError: unknown) => {
        if (active) setError(loadError instanceof Error ? loadError.message : String(loadError));
      })
      .finally(() => {
        if (active) setLoadingDetails(false);
      });
    return () => {
      active = false;
    };
  }, [selectedId]);

  const contextTotal = useMemo(() => {
    if (!diagnostics) return 0;
    return CONTEXT_PARTS.reduce(
      (total, part) => total + diagnostics.context.latest_breakdown[part.key],
      0
    );
  }, [diagnostics]);

  return (
    <div className="flex h-full min-h-0 flex-col" style={{ color: 'var(--text-primary)' }}>
      <div
        className="flex h-12 shrink-0 items-center justify-between border-b px-4"
        style={{ borderColor: 'var(--border-subtle)' }}
      >
        <div className="flex items-center gap-2">
          <CircleGauge size={16} />
          <h2 className="text-sm font-semibold">Run diagnostics</h2>
          <span className="text-xs" style={{ color: 'var(--text-tertiary)' }}>
            {runs.length}
          </span>
        </div>
        <button
          type="button"
          onClick={() => void loadRuns()}
          className="flex size-8 items-center justify-center rounded-md"
          style={{ background: 'var(--bg-hover)', color: 'var(--text-secondary)' }}
          title="Refresh diagnostics"
        >
          <RefreshCw size={14} className={loadingRuns ? 'animate-spin' : ''} />
        </button>
      </div>

      {error && (
        <div
          className="flex items-center gap-2 border-b px-4 py-2 text-xs text-red-500"
          style={{ borderColor: 'var(--border-subtle)' }}
        >
          <AlertTriangle size={14} />
          <span className="min-w-0 break-words">{error}</span>
        </div>
      )}

      <div className="flex min-h-0 flex-1 flex-col md:flex-row">
        <aside
          className="max-h-44 w-full shrink-0 overflow-y-auto border-b md:max-h-none md:w-72 md:border-r md:border-b-0"
          style={{ borderColor: 'var(--border-subtle)', background: 'var(--bg-secondary)' }}
        >
          {runs.length === 0 && !loadingRuns ? (
            <div
              className="px-4 py-8 text-center text-xs"
              style={{ color: 'var(--text-tertiary)' }}
            >
              No durable runs
            </div>
          ) : (
            runs.map((run) => {
              const selected = run.diagnostic_id === selectedId;
              return (
                <button
                  type="button"
                  key={run.diagnostic_id}
                  onClick={() => setSelectedId(run.diagnostic_id)}
                  className="flex w-full items-start gap-2 border-b px-3 py-3 text-left"
                  style={{
                    borderColor: 'var(--border-subtle)',
                    background: selected ? 'var(--bg-active)' : 'transparent',
                  }}
                >
                  <span className="mt-0.5 shrink-0">{statusIcon(run.status)}</span>
                  <span className="min-w-0 flex-1">
                    <span className="block truncate text-xs font-medium">
                      {run.input_preview || shortId(run.diagnostic_id)}
                    </span>
                    <span
                      className="mt-1 flex items-center gap-2 text-[11px]"
                      style={{ color: 'var(--text-tertiary)' }}
                    >
                      <span>{formatTime(run.started_at)}</span>
                      <span>{run.trace_count} traces</span>
                    </span>
                    <span
                      className="mt-1 block truncate font-mono text-[10px]"
                      style={{ color: 'var(--text-tertiary)' }}
                    >
                      {run.models.join(', ') || 'model unknown'}
                    </span>
                  </span>
                  <ChevronRight size={14} className="mt-0.5 shrink-0 opacity-50" />
                </button>
              );
            })
          )}
        </aside>

        <main className="min-w-0 flex-1 overflow-y-auto">
          {loadingDetails && !diagnostics ? (
            <div className="flex h-full items-center justify-center">
              <RefreshCw size={18} className="animate-spin" />
            </div>
          ) : diagnostics ? (
            <div className="mx-auto max-w-6xl px-5 py-5">
              <div className="flex flex-wrap items-start justify-between gap-3">
                <div className="min-w-0">
                  <div className="flex items-center gap-2">
                    <h3 className="truncate font-mono text-sm font-semibold">
                      {diagnostics.diagnostic_id}
                    </h3>
                    {loadingDetails && <RefreshCw size={13} className="animate-spin" />}
                  </div>
                  <div className="mt-1 text-xs" style={{ color: 'var(--text-tertiary)' }}>
                    {diagnostics.traces.length} trace invocations
                    {diagnostics.parent_run_id ? ` · parent ${diagnostics.parent_run_id}` : ''}
                  </div>
                </div>
              </div>

              <div className="mt-4 grid grid-cols-2 gap-2 md:grid-cols-3 xl:grid-cols-6">
                <Stat
                  label="Provider input"
                  value={formatNumber(diagnostics.usage.total_input_tokens)}
                />
                <Stat
                  label="Provider output"
                  value={formatNumber(diagnostics.usage.total_output_tokens)}
                />
                <Stat
                  label="Cache read"
                  value={formatNumber(diagnostics.usage.total_cached_input_tokens)}
                />
                <Stat
                  label="Cache write"
                  value={formatNumber(diagnostics.usage.total_cache_creation_input_tokens)}
                />
                <Stat label="Read rate" value={formatPercent(diagnostics.cache.read_rate)} />
                <Stat
                  label="Missing usage"
                  value={formatNumber(diagnostics.usage.calls_missing_usage)}
                />
              </div>

              {diagnostics.issues.length > 0 && (
                <section
                  className="mt-6 border-t pt-4"
                  style={{ borderColor: 'var(--border-subtle)' }}
                >
                  <h4 className="flex items-center gap-2 text-xs font-semibold">
                    <AlertTriangle size={14} /> Issues
                  </h4>
                  <div className="mt-2 space-y-1">
                    {diagnostics.issues.map((issue) => (
                      <div
                        key={`${issue.kind}-${issue.message}`}
                        className="flex gap-2 py-1.5 text-xs"
                        style={{
                          color:
                            issue.severity === 'critical'
                              ? '#ef4444'
                              : issue.severity === 'warning'
                                ? '#f59e0b'
                                : 'var(--text-secondary)',
                        }}
                      >
                        <span className="w-16 shrink-0 font-mono uppercase">{issue.severity}</span>
                        <span>{issue.message}</span>
                      </div>
                    ))}
                  </div>
                </section>
              )}

              <section
                className="mt-6 border-t pt-4"
                style={{ borderColor: 'var(--border-subtle)' }}
              >
                <div className="flex flex-wrap items-center justify-between gap-2">
                  <h4 className="flex items-center gap-2 text-xs font-semibold">
                    <Database size={14} /> Context
                  </h4>
                  <div className="font-mono text-xs" style={{ color: 'var(--text-secondary)' }}>
                    estimated {formatNumber(diagnostics.context.latest_estimated_context_tokens)} /{' '}
                    {formatNumber(diagnostics.context.latest_context_limit_tokens)}
                  </div>
                </div>
                <div
                  className="mt-3 flex h-2 w-full overflow-hidden rounded-sm"
                  style={{ background: 'var(--bg-hover)' }}
                >
                  {CONTEXT_PARTS.map((part) => {
                    const value = diagnostics.context.latest_breakdown[part.key];
                    return (
                      <div
                        key={part.key}
                        title={`${part.label}: ${formatNumber(value)}`}
                        style={{
                          width: `${contextTotal > 0 ? (value / contextTotal) * 100 : 0}%`,
                          background: part.color,
                        }}
                      />
                    );
                  })}
                </div>
                <div className="mt-3 grid grid-cols-2 gap-x-6 gap-y-2 md:grid-cols-3">
                  {CONTEXT_PARTS.map((part) => (
                    <div key={part.key} className="flex items-center justify-between gap-3 text-xs">
                      <span
                        className="flex min-w-0 items-center gap-2"
                        style={{ color: 'var(--text-secondary)' }}
                      >
                        <span
                          className="size-2 shrink-0 rounded-sm"
                          style={{ background: part.color }}
                        />
                        <span className="truncate">{part.label}</span>
                      </span>
                      <span className="font-mono">
                        {formatNumber(diagnostics.context.latest_breakdown[part.key])}
                      </span>
                    </div>
                  ))}
                </div>
                <div
                  className="mt-3 flex flex-wrap gap-x-6 gap-y-1 text-xs"
                  style={{ color: 'var(--text-tertiary)' }}
                >
                  <span>
                    Provider input: {formatNumber(diagnostics.context.latest_provider_input_tokens)}
                  </span>
                  <span>
                    Protected max: {formatNumber(diagnostics.context.max_protected_context_tokens)}
                  </span>
                  <span>{diagnostics.context.max_protected_message_count} protected messages</span>
                </div>
              </section>

              <section
                className="mt-6 border-t pt-4"
                style={{ borderColor: 'var(--border-subtle)' }}
              >
                <h4 className="flex items-center gap-2 text-xs font-semibold">
                  <Sparkles size={14} /> Cache stability
                </h4>
                <div className="mt-3 grid grid-cols-3 gap-2">
                  <Stat
                    label="Stable prefix changes"
                    value={formatNumber(diagnostics.cache.stable_prefix_hash_changes)}
                  />
                  <Stat
                    label="System changes"
                    value={formatNumber(diagnostics.cache.system_prefix_hash_changes)}
                  />
                  <Stat
                    label="Tool schema changes"
                    value={formatNumber(diagnostics.cache.tools_schema_hash_changes)}
                  />
                </div>
              </section>

              {diagnostics.compressions.length > 0 && (
                <section
                  className="mt-6 border-t pt-4"
                  style={{ borderColor: 'var(--border-subtle)' }}
                >
                  <h4 className="flex items-center gap-2 text-xs font-semibold">
                    <Braces size={14} /> Compressions
                  </h4>
                  <div className="mt-2 divide-y" style={{ borderColor: 'var(--border-subtle)' }}>
                    {diagnostics.compressions.map((item) => (
                      <div
                        key={`${item.trace_run_id}-${item.sequence}`}
                        className="grid grid-cols-4 gap-3 py-2 text-xs"
                      >
                        <span className="truncate font-mono">{item.source}</span>
                        <span>
                          {formatNumber(item.before_tokens)} → {formatNumber(item.after_tokens)}{' '}
                          tokens
                        </span>
                        <span>
                          {item.before_messages} → {item.after_messages} messages
                        </span>
                        <span>{formatNumber(item.protected_context_tokens)} protected</span>
                      </div>
                    ))}
                  </div>
                </section>
              )}

              {diagnostics.prompt_assembly && (
                <section
                  className="mt-6 border-t pt-4"
                  style={{ borderColor: 'var(--border-subtle)' }}
                >
                  <div className="flex items-center justify-between gap-3">
                    <h4 className="flex items-center gap-2 text-xs font-semibold">
                      <ShieldCheck size={14} /> Prompt modules
                    </h4>
                    <span className="font-mono text-xs">
                      {formatNumber(diagnostics.prompt_assembly.estimated_tokens)} tokens
                    </span>
                  </div>
                  <div className="mt-2 divide-y" style={{ borderColor: 'var(--border-subtle)' }}>
                    {diagnostics.prompt_assembly.modules.map((module) => (
                      <div
                        key={module.name}
                        className="grid grid-cols-[minmax(0,1fr)_auto_auto_auto] items-center gap-3 py-2 text-xs"
                      >
                        <span className="min-w-0 truncate font-medium">{module.name}</span>
                        <span className="font-mono">{formatNumber(module.estimated_tokens)}</span>
                        <span
                          style={{
                            color: module.stable_prefix ? '#22c55e' : 'var(--text-tertiary)',
                          }}
                        >
                          {module.stable_prefix ? 'stable' : 'dynamic'}
                        </span>
                        <span className="font-mono" title={module.content_hash}>
                          {module.content_hash ? shortId(module.content_hash) : 'excluded'}
                          {module.truncated ? ' · truncated' : ''}
                        </span>
                      </div>
                    ))}
                  </div>
                </section>
              )}

              <section
                className="mt-6 border-t pt-4"
                style={{ borderColor: 'var(--border-subtle)' }}
              >
                <h4 className="flex items-center gap-2 text-xs font-semibold">
                  <Clock3 size={14} /> Trace invocations
                </h4>
                <div className="mt-2 space-y-4">
                  {diagnostics.traces.map((trace) => (
                    <div
                      key={trace.trace_run_id}
                      className="border-l-2 pl-3"
                      style={{ borderColor: 'var(--border-strong)' }}
                    >
                      <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-xs">
                        {statusIcon(trace.status)}
                        <span className="font-semibold">{trace.agent_name || 'agent'}</span>
                        <span className="font-mono">{trace.model || 'model unknown'}</span>
                        <span style={{ color: 'var(--text-tertiary)' }}>
                          {trace.provider || 'provider unknown'}
                        </span>
                        <span className="font-mono" title={trace.trace_run_id}>
                          {shortId(trace.trace_run_id)}
                        </span>
                      </div>
                      <div className="mt-2 overflow-x-auto">
                        <table className="w-full min-w-[760px] text-left text-[11px]">
                          <thead style={{ color: 'var(--text-tertiary)' }}>
                            <tr>
                              <th className="pb-1 font-medium">Call</th>
                              <th className="pb-1 font-medium">Source</th>
                              <th className="pb-1 font-medium">Input</th>
                              <th className="pb-1 font-medium">Output</th>
                              <th className="pb-1 font-medium">Cached</th>
                              <th className="pb-1 font-medium">Estimated context</th>
                              <th className="pb-1 font-medium">Protected</th>
                              <th className="pb-1 font-medium">Stable prefix</th>
                              <th className="pb-1 font-medium">Tools</th>
                              <th className="pb-1 font-medium">Duration</th>
                            </tr>
                          </thead>
                          <tbody>
                            {trace.llm_calls.map((call) => (
                              <tr
                                key={call.sequence}
                                className="border-t"
                                style={{ borderColor: 'var(--border-subtle)' }}
                              >
                                <td className="py-1.5 font-mono">#{call.sequence}</td>
                                <td className="py-1.5">{call.source}</td>
                                <td className="py-1.5 font-mono">
                                  {formatNumber(call.input_tokens)}
                                </td>
                                <td className="py-1.5 font-mono">
                                  {formatNumber(call.output_tokens)}
                                </td>
                                <td className="py-1.5 font-mono">
                                  {formatNumber(call.cached_input_tokens)}
                                </td>
                                <td className="py-1.5 font-mono">
                                  {formatNumber(call.estimated_context_tokens)}
                                </td>
                                <td className="py-1.5 font-mono">
                                  {formatNumber(call.protected_context_tokens)}
                                </td>
                                <td className="py-1.5 font-mono" title={call.stable_prefix_hash}>
                                  {shortId(call.stable_prefix_hash)}
                                </td>
                                <td className="py-1.5 font-mono" title={call.tools_schema_hash}>
                                  {shortId(call.tools_schema_hash)}
                                </td>
                                <td className="py-1.5 font-mono">
                                  {formatNumber(call.duration_ms)} ms
                                </td>
                              </tr>
                            ))}
                          </tbody>
                        </table>
                      </div>
                    </div>
                  ))}
                </div>
              </section>
            </div>
          ) : (
            <div
              className="flex h-full items-center justify-center text-xs"
              style={{ color: 'var(--text-tertiary)' }}
            >
              Select a durable run
            </div>
          )}
        </main>
      </div>
    </div>
  );
}
