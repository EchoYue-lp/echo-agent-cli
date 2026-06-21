import { useEffect, useState } from 'react';
import { Activity, Cpu, RefreshCw, Server, TrendingUp } from 'lucide-react';
import { taskRuntimeApi } from '../../api/endpoints';

interface RunSummary {
  run_id?: string;
  total_input_tokens: number;
  total_output_tokens: number;
  total_cached_input_tokens: number;
  total_cache_creation_input_tokens: number;
  cache_read_rate: number;
  llm_calls: number;
  model_breakdown: {
    model: string;
    llm_calls: number;
    input_tokens: number;
    output_tokens: number;
    cached_input_tokens: number;
  }[];
  top_low_hit_reasons: string[];
}

function formatRate(value: number): string {
  return `${(value * 100).toFixed(1)}%`;
}

export function UsageTrendsPanel() {
  const [summaries, setSummaries] = useState<RunSummary[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const load = async () => {
    setLoading(true);
    setError(null);
    try {
      // Load recent usage records with a reasonable window.
      // Backend supports filtering by time/model/route_kind; for trends we
      // pull the last 24h and let the backend handle the aggregation.
      const oneDayAgo = new Date(Date.now() - 24 * 60 * 60 * 1000).toISOString();
      const records = await taskRuntimeApi.queryUsageRecords({
        limit: 500,
        created_after: oneDayAgo,
      });
      // Group by run_id
      const byRun = new Map<string, Record<string, unknown>[]>();
      for (const r of records) {
        const key = (r.run_id as string) || '__no_run__';
        if (!byRun.has(key)) byRun.set(key, []);
        byRun.get(key)!.push(r);
      }
      // Compute per-run summaries
      const items: RunSummary[] = [];
      for (const [runId, recs] of byRun) {
        let totalIn = 0, totalOut = 0, totalCached = 0, totalCacheWrite = 0;
        const modelMap = new Map<string, { calls: number; inp: number; out: number; cached: number }>();
        for (const r of recs) {
          const inp = (r.input_tokens as number) || 0;
          const out = (r.output_tokens as number) || 0;
          const cached = (r.cached_input_tokens as number) || 0;
          const cw = (r.cache_creation_input_tokens as number) || 0;
          totalIn += inp;
          totalOut += out;
          totalCached += cached;
          totalCacheWrite += cw;
          const model = (r.model as string) || 'unknown';
          if (!modelMap.has(model)) modelMap.set(model, { calls: 0, inp: 0, out: 0, cached: 0 });
          const m = modelMap.get(model)!;
          m.calls += 1;
          m.inp += inp;
          m.out += out;
          m.cached += cached;
        }
        items.push({
          run_id: runId === '__no_run__' ? undefined : runId,
          total_input_tokens: totalIn,
          total_output_tokens: totalOut,
          total_cached_input_tokens: totalCached,
          total_cache_creation_input_tokens: totalCacheWrite,
          cache_read_rate: totalIn > 0 ? totalCached / totalIn : 0,
          llm_calls: recs.length,
          model_breakdown: [...modelMap.entries()].map(([model, m]) => ({
            model,
            llm_calls: m.calls,
            input_tokens: m.inp,
            output_tokens: m.out,
            cached_input_tokens: m.cached,
          })),
          top_low_hit_reasons: [],
        });
      }
      items.sort((a, b) => b.llm_calls - a.llm_calls);
      setSummaries(items);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    void load();
  }, []);

  const totalInput = summaries.reduce((s, r) => s + r.total_input_tokens, 0);
  const totalCached = summaries.reduce((s, r) => s + r.total_cached_input_tokens, 0);
  const totalCalls = summaries.reduce((s, r) => s + r.llm_calls, 0);

  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="mb-4 flex items-center justify-between gap-3">
        <div>
          <h2 className="text-sm font-semibold" style={{ color: 'var(--text-primary)' }}>
            Usage 趋势
          </h2>
          <p className="mt-1 text-xs" style={{ color: 'var(--text-tertiary)' }}>
            按 Run / Model 聚合 token 消耗和 cache 命中趋势。
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
        <div className="mb-3 rounded-md px-3 py-2 text-xs" style={{ background: 'var(--bg-hover)', color: 'var(--color-error)' }}>
          {error}
        </div>
      )}

      {/* Aggregate gauges */}
      <div className="grid grid-cols-4 gap-2 mb-4">
        <Metric label="Total LLM calls" value={totalCalls.toLocaleString()} icon={<Cpu size={14} />} />
        <Metric label="Total input" value={totalInput.toLocaleString()} icon={<Activity size={14} />} />
        <Metric label="Overall cache read" value={formatRate(totalInput > 0 ? totalCached / totalInput : 0)} icon={<TrendingUp size={14} />} />
        <Metric label="Runs" value={summaries.length.toLocaleString()} icon={<Server size={14} />} />
      </div>

      {/* Per-run breakdown */}
      <div className="min-h-0 flex-1 overflow-auto">
        <div className="mb-2 text-xs font-semibold" style={{ color: 'var(--text-primary)' }}>
          Run 用量明细
        </div>
        <div className="space-y-2">
          {summaries.map((s, i) => (
            <div key={i} className="rounded-lg border p-3" style={{ borderColor: 'var(--border-secondary)', background: 'var(--bg-secondary)' }}>
              <div className="flex items-center justify-between mb-2">
                <span className="text-xs font-mono font-medium" style={{ color: 'var(--text-primary)' }}>
                  {s.run_id ? s.run_id.slice(0, 16) + '...' : '(no run)'}
                </span>
                <span className="text-[11px]" style={{ color: 'var(--text-tertiary)' }}>
                  {s.llm_calls} calls · {formatRate(s.cache_read_rate)} cache read
                </span>
              </div>
              <div className="grid grid-cols-3 gap-2 text-[11px]">
                <div style={{ color: 'var(--text-tertiary)' }}>
                  Input: <span style={{ color: 'var(--text-primary)' }}>{s.total_input_tokens.toLocaleString()}</span>
                </div>
                <div style={{ color: 'var(--text-tertiary)' }}>
                  Output: <span style={{ color: 'var(--text-primary)' }}>{s.total_output_tokens.toLocaleString()}</span>
                </div>
                <div style={{ color: 'var(--text-tertiary)' }}>
                  Cached: <span style={{ color: 'var(--text-primary)' }}>{s.total_cached_input_tokens.toLocaleString()}</span>
                </div>
              </div>
              {s.model_breakdown.length > 1 && (
                <div className="mt-2 flex flex-wrap gap-1">
                  {s.model_breakdown.map((m, j) => (
                    <span key={j} className="rounded px-1.5 py-0.5 text-[10px] font-mono" style={{ background: 'var(--bg-hover)', color: 'var(--text-secondary)' }}>
                      {m.model}: {m.llm_calls} calls
                    </span>
                  ))}
                </div>
              )}
            </div>
          ))}
          {summaries.length === 0 && !loading && (
            <div className="rounded-md p-6 text-center text-xs" style={{ color: 'var(--text-tertiary)', background: 'var(--bg-secondary)' }}>
              暂无 usage 数据。发送消息或执行 TaskRuntime 后会在这里显示。
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

function Metric({ label, value, icon }: { label: string; value: string; icon: React.ReactNode }) {
  return (
    <div className="rounded-lg p-3" style={{ background: 'var(--bg-secondary)' }}>
      <div className="mb-1 flex items-center gap-1.5 text-[10px]" style={{ color: 'var(--text-tertiary)' }}>
        {icon} {label}
      </div>
      <div className="truncate font-mono text-sm font-semibold" style={{ color: 'var(--text-primary)' }}>
        {value}
      </div>
    </div>
  );
}
