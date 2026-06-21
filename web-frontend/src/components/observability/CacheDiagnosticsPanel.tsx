import { useEffect, useState } from 'react';
import { AlertTriangle, CheckCircle, Info, RefreshCw, ShieldAlert, TrendingUp } from 'lucide-react';
import { traceEventsApi, type CacheDiagnosticsData } from '../../api/endpoints';

function severityIcon(severity: string) {
  switch (severity) {
    case 'critical':
      return <ShieldAlert size={14} style={{ color: 'var(--color-error)' }} />;
    case 'warning':
      return <AlertTriangle size={14} style={{ color: 'var(--color-warning)' }} />;
    default:
      return <Info size={14} style={{ color: 'var(--color-info)' }} />;
  }
}

function severityBg(severity: string): string {
  switch (severity) {
    case 'critical':
      return 'var(--color-error-bg)';
    case 'warning':
      return 'var(--color-warning-bg)';
    default:
      return 'var(--color-info-bg)';
  }
}

function formatRate(value: number): string {
  return `${(value * 100).toFixed(1)}%`;
}

function hashPreview(hash?: string | null): string {
  if (!hash) return '—';
  return hash.length > 16 ? `${hash.slice(0, 16)}...` : hash;
}

export function CacheDiagnosticsPanel() {
  const [data, setData] = useState<CacheDiagnosticsData | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const load = async () => {
    setLoading(true);
    setError(null);
    try {
      setData(await traceEventsApi.getCacheDiagnostics());
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    void load();
  }, []);

  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="mb-4 flex items-center justify-between gap-3">
        <div>
          <h2 className="text-sm font-semibold" style={{ color: 'var(--text-primary)' }}>
            缓存诊断
          </h2>
          <p className="mt-1 text-xs" style={{ color: 'var(--text-tertiary)' }}>
            分析为什么 prompt cache 命中率低，并给出修复建议。
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

      {data && (
        <div className="min-h-0 flex-1 overflow-auto space-y-4">
          {/* Cache Rate Gauge */}
          <div
            className="rounded-lg border p-4"
            style={{ borderColor: 'var(--border-secondary)', background: 'var(--bg-secondary)' }}
          >
            <div className="flex items-center gap-2 text-xs font-semibold mb-2" style={{ color: 'var(--text-primary)' }}>
              <TrendingUp size={14} /> 当前缓存命中率
            </div>
            <div className="flex items-end gap-2">
              <span className="text-2xl font-bold font-mono" style={{ color: data.overall_read_rate > 0.3 ? 'var(--color-success)' : 'var(--color-error)' }}>
                {formatRate(data.overall_read_rate)}
              </span>
              <span className="text-xs" style={{ color: 'var(--text-tertiary)' }}>
                {data.total_cached_input_tokens.toLocaleString()} / {data.total_input_tokens.toLocaleString()} tokens
              </span>
            </div>
            <div className="mt-3 grid grid-cols-3 gap-2 text-[11px]">
              <div style={{ color: 'var(--text-tertiary)' }}>
                LLM calls: <span style={{ color: 'var(--text-primary)' }}>{data.total_llm_calls}</span>
              </div>
              <div style={{ color: 'var(--text-tertiary)' }}>
                Missing usage: <span style={{ color: data.calls_missing_usage > 0 ? 'var(--color-error)' : 'var(--text-primary)' }}>{data.calls_missing_usage}</span>
              </div>
              <div style={{ color: 'var(--text-tertiary)' }}>
                Cache write: <span style={{ color: 'var(--text-primary)' }}>{data.total_cache_creation_input_tokens.toLocaleString()}</span>
              </div>
            </div>
          </div>

          {/* Issues */}
          {data.issues.length > 0 && (
            <section>
              <div className="mb-2 text-xs font-semibold" style={{ color: 'var(--text-primary)' }}>
                诊断问题
              </div>
              <div className="space-y-2">
                {data.issues.map((issue, i) => (
                  <div
                    key={i}
                    className="rounded-lg border p-3"
                    style={{ borderColor: 'var(--border-secondary)', background: severityBg(issue.severity) }}
                  >
                    <div className="flex items-start gap-2">
                      {severityIcon(issue.severity)}
                      <div className="min-w-0">
                        <div className="text-xs font-medium" style={{ color: 'var(--text-primary)' }}>
                          {issue.kind.replace(/_/g, ' ')}
                        </div>
                        <div className="mt-1 text-xs leading-relaxed" style={{ color: 'var(--text-secondary)' }}>
                          {issue.message}
                        </div>
                        <div className="mt-1 text-[11px]" style={{ color: 'var(--text-tertiary)' }}>
                          影响 {issue.affected_calls} 次调用
                        </div>
                      </div>
                    </div>
                  </div>
                ))}
              </div>
            </section>
          )}

          {/* Suggested Fixes */}
          {data.suggested_fixes.length > 0 && (
            <section>
              <div className="mb-2 flex items-center gap-2 text-xs font-semibold" style={{ color: 'var(--text-primary)' }}>
                <CheckCircle size={14} /> 修复建议
              </div>
              <div className="space-y-1.5">
                {data.suggested_fixes.map((fix, i) => (
                  <div
                    key={i}
                    className="rounded-md px-3 py-2 text-xs"
                    style={{ background: 'var(--bg-secondary)', color: 'var(--text-secondary)' }}
                  >
                    {i + 1}. {fix}
                  </div>
                ))}
              </div>
            </section>
          )}

          {/* Recent Calls Comparison */}
          {data.recent_calls && data.recent_calls.length > 0 && (
            <section>
              <div className="mb-2 text-xs font-semibold" style={{ color: 'var(--text-primary)' }}>
                最近 {data.recent_calls.length} 次 LLM 调用对比
              </div>
              <div className="overflow-auto rounded-lg border" style={{ borderColor: 'var(--border-primary)' }}>
                <table className="w-full text-[11px]">
                  <thead>
                    <tr style={{ background: 'var(--bg-hover)' }}>
                      <th className="px-2 py-1.5 text-left" style={{ color: 'var(--text-tertiary)' }}>Model</th>
                      <th className="px-2 py-1.5 text-right" style={{ color: 'var(--text-tertiary)' }}>Input</th>
                      <th className="px-2 py-1.5 text-right" style={{ color: 'var(--text-tertiary)' }}>Cached</th>
                      <th className="px-2 py-1.5 text-left" style={{ color: 'var(--text-tertiary)' }}>System Hash</th>
                      <th className="px-2 py-1.5 text-left" style={{ color: 'var(--text-tertiary)' }}>Tools Hash</th>
                      <th className="px-2 py-1.5 text-left" style={{ color: 'var(--text-tertiary)' }}>CWD Hash</th>
                    </tr>
                  </thead>
                  <tbody>
                    {data.recent_calls.map((call, i) => (
                      <tr
                        key={i}
                        className="border-t"
                        style={{ borderColor: 'var(--border-secondary)' }}
                      >
                        <td className="px-2 py-1.5 font-mono" style={{ color: 'var(--text-primary)' }}>{call.model}</td>
                        <td className="px-2 py-1.5 text-right font-mono" style={{ color: 'var(--text-secondary)' }}>{call.input_tokens.toLocaleString()}</td>
                        <td className="px-2 py-1.5 text-right font-mono" style={{ color: 'var(--text-secondary)' }}>{call.cached_input_tokens.toLocaleString()}</td>
                        <td className="px-2 py-1.5 font-mono" style={{ color: 'var(--text-tertiary)' }} title={call.system_prompt_hash ?? undefined}>
                          {hashPreview(call.system_prompt_hash)}
                        </td>
                        <td className="px-2 py-1.5 font-mono" style={{ color: 'var(--text-tertiary)' }} title={call.tools_schema_hash ?? undefined}>
                          {hashPreview(call.tools_schema_hash)}
                        </td>
                        <td className="px-2 py-1.5 font-mono" style={{ color: 'var(--text-tertiary)' }} title={call.cwd_hash ?? undefined}>
                          {hashPreview(call.cwd_hash)}
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            </section>
          )}

          {!data && !loading && (
            <div className="rounded-md p-6 text-center text-xs" style={{ color: 'var(--text-tertiary)', background: 'var(--bg-secondary)' }}>
              暂无缓存诊断数据。发送消息后再查看。
            </div>
          )}
        </div>
      )}
    </div>
  );
}
