import { useMemo } from 'react';
import type { TraceEvent } from '../../api/endpoints';

interface TokenUsageChartProps {
  events: TraceEvent[];
}

interface LlmCallData {
  index: number;
  model: string;
  input: number;
  output: number;
  cached: number;
  cacheWrite: number;
  usageReported: boolean;
  total: number;
}

export function TokenUsageChart({ events }: TokenUsageChartProps) {
  const calls = useMemo<LlmCallData[]>(() => {
    let idx = 0;
    return events
      .filter((e) => e.kind.type === 'llm_call')
      .map((e) => {
        const k = e.kind;
        if (k.type !== 'llm_call') return null;
        return {
          index: ++idx,
          model: k.model,
          input: k.input_tokens,
          output: k.output_tokens,
          cached: k.cached_input_tokens,
          cacheWrite: k.cache_creation_input_tokens,
          usageReported: k.usage_reported,
          total: k.input_tokens + k.output_tokens,
        };
      })
      .filter((x): x is LlmCallData => x !== null);
  }, [events]);

  const totals = useMemo(() => {
    const input = calls.reduce((sum, c) => sum + c.input, 0);
    const output = calls.reduce((sum, c) => sum + c.output, 0);
    const cached = calls.reduce((sum, c) => sum + c.cached, 0);
    const cacheWrite = calls.reduce((sum, c) => sum + c.cacheWrite, 0);
    const missingUsage = calls.filter((c) => !c.usageReported).length;
    const cacheReadRate = input > 0 ? cached / input : 0;
    return { input, output, cached, cacheWrite, missingUsage, cacheReadRate, total: input + output };
  }, [calls]);

  const maxTotal = useMemo(() => Math.max(1, ...calls.map((c) => c.total)), [calls]);

  const s = {
    text: 'var(--text-primary)',
    textSec: 'var(--text-secondary)',
    textTer: 'var(--text-tertiary)',
    border: 'var(--border-primary)',
    bg: 'var(--bg-primary)',
    bgHover: 'var(--bg-hover)',
  };

  const inputColor = 'var(--color-info, #3b82f6)';
  const outputColor = 'var(--color-success, #22c55e)';
  const cachedColor = 'var(--color-warning, #f59e0b)';

  if (calls.length === 0) {
    return (
      <div className="py-8 text-center text-xs" style={{ color: s.textTer }}>
        No LLM calls recorded
      </div>
    );
  }

  return (
    <div className="space-y-3">
      {/* Summary */}
      <div className="grid grid-cols-3 gap-2">
        <SummaryCard label="Input" value={totals.input} color={inputColor} />
        <SummaryCard label="Output" value={totals.output} color={outputColor} />
        <SummaryCard label="Cached" value={totals.cached} color={cachedColor} />
        <SummaryCard label="Cache write" value={totals.cacheWrite} color={s.textSec} />
        <SummaryCard
          label="Cache read"
          value={`${(totals.cacheReadRate * 100).toFixed(1)}%`}
          color={cachedColor}
        />
        <SummaryCard label="Missing usage" value={totals.missingUsage} color={s.textTer} />
      </div>

      {/* Legend */}
      <div className="flex items-center gap-4 px-1">
        <span className="flex items-center gap-1 text-[10px]" style={{ color: s.textSec }}>
          <span
            className="inline-block w-2.5 h-2.5 rounded-sm"
            style={{ background: inputColor }}
          />
          Input
        </span>
        <span className="flex items-center gap-1 text-[10px]" style={{ color: s.textSec }}>
          <span
            className="inline-block w-2.5 h-2.5 rounded-sm"
            style={{ background: outputColor }}
          />
          Output
        </span>
        <span className="flex items-center gap-1 text-[10px]" style={{ color: s.textSec }}>
          <span
            className="inline-block w-2.5 h-2.5 rounded-sm"
            style={{ background: cachedColor }}
          />
          Cached input
        </span>
      </div>

      {/* Bars */}
      <div className="space-y-1.5">
        {calls.map((call) => {
          const inputWidth = (call.input / maxTotal) * 100;
          const outputWidth = (call.output / maxTotal) * 100;

          return (
            <div
              key={call.index}
              className="rounded-lg border px-3 py-2"
              style={{ borderColor: s.border, background: s.bg }}
            >
              <div className="flex items-center justify-between mb-1">
                <span className="text-[10px] font-mono" style={{ color: s.textTer }}>
                  #{call.index} {call.model}
                </span>
                <span className="text-[10px] font-mono" style={{ color: s.textTer }}>
                  {call.total.toLocaleString()} tokens
                  {!call.usageReported ? ' · usage missing' : ''}
                </span>
              </div>
              <div
                className="flex h-3 w-full rounded-full overflow-hidden"
                style={{ background: s.bgHover }}
              >
                <div
                  className="h-full"
                  style={{ width: `${inputWidth}%`, background: inputColor, opacity: 0.8 }}
                />
                <div
                  className="h-full"
                  style={{ width: `${outputWidth}%`, background: outputColor, opacity: 0.8 }}
                />
              </div>
              <div className="flex justify-between mt-0.5">
                <span className="text-[9px]" style={{ color: s.textTer }}>
                  {call.input.toLocaleString()} in
                  {call.cached > 0 ? ` · ${call.cached.toLocaleString()} cached` : ''}
                  {call.cacheWrite > 0 ? ` · ${call.cacheWrite.toLocaleString()} written` : ''}
                </span>
                <span className="text-[9px]" style={{ color: s.textTer }}>
                  {call.output.toLocaleString()} out
                </span>
              </div>
            </div>
          );
        })}
      </div>
    </div>
  );
}

function SummaryCard({
  label,
  value,
  color,
}: {
  label: string;
  value: number | string;
  color: string;
}) {
  return (
    <div className="rounded-lg p-2 text-center" style={{ background: 'var(--bg-hover)' }}>
      <div className="text-sm font-semibold font-mono" style={{ color }}>
        {typeof value === 'number' ? value.toLocaleString() : value}
      </div>
      <div className="text-[10px]" style={{ color: 'var(--text-tertiary)' }}>
        {label}
      </div>
    </div>
  );
}
