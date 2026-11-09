import { useEffect, useRef, useState } from 'react';
import type { Result as ChartResult, VisualizationSpec } from 'vega-embed';
import { Maximize2, Minimize2 } from 'lucide-react';

interface ChartCardProps {
  spec: unknown;
  standalone?: boolean;
}

/** Auto-detect if a tool result string contains a vega-lite spec */
function extractVegaLiteSpec(result: string): unknown | null {
  try {
    const parsed = JSON.parse(result);
    if (
      parsed &&
      typeof parsed === 'object' &&
      typeof parsed.$schema === 'string' &&
      parsed.$schema.includes('vega-lite')
    ) {
      return parsed;
    }
  } catch {
    // Not JSON — try extracting JSON from markdown code blocks
    const fenced = result.match(/```(?:json)?\s*([\s\S]*?)```/);
    if (fenced) {
      try {
        const parsed = JSON.parse(fenced[1]);
        if (
          parsed &&
          typeof parsed === 'object' &&
          typeof parsed.$schema === 'string' &&
          parsed.$schema.includes('vega-lite')
        ) {
          return parsed;
        }
      } catch {
        // not valid JSON
      }
    }
  }
  return null;
}

export { extractVegaLiteSpec };

export function ChartCard({ spec }: ChartCardProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const [expanded, setExpanded] = useState(false);
  const [state, setState] = useState<'loading' | 'ready' | 'error'>('loading');
  const [error, setError] = useState('');

  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;
    const surface = document.createElement('div');
    container.replaceChildren(surface);
    let disposed = false;
    let chart: ChartResult | undefined;
    // 本地模块与官方AST解释器保留既有spec语义，适用桌面原CSP；不依赖CDN、
    // srcdoc内联脚本或unsafe-eval。每次渲染有独立surface，迟到结果不能污染新图。
    void (async () => {
      try {
        // 同一提交内已卸载的卡片不再启动体积较大的图表模块加载。
        await Promise.resolve();
        if (disposed) return;
        const [{ default: embed }, { expressionInterpreter }] = await Promise.all([
          import('vega-embed'),
          import('vega-interpreter'),
        ]);
        if (disposed) return;
        setState('loading');
        setError('');
        const result = await embed(surface, spec as VisualizationSpec, {
          renderer: 'svg',
          ast: true,
          expr: expressionInterpreter,
          actions: { export: true, source: false, compiled: false, editor: false },
        });
        if (disposed) {
          result.finalize();
          return;
        }
        chart = result;
        setState('ready');
      } catch (failure) {
        if (!disposed) {
          setError(failure instanceof Error ? failure.message : '图表绘制失败');
          setState('error');
        }
      }
    })();
    return () => {
      disposed = true;
      chart?.finalize();
      surface.remove();
    };
  }, [spec]);

  return (
    <div
      className="min-w-0 overflow-hidden rounded-lg border border-[var(--border-primary)] bg-[var(--bg-primary)]"
      data-chart-state={state}
    >
      <div className="flex items-center justify-between border-b border-[var(--border-primary)] bg-[var(--bg-secondary)] px-4 py-2">
        <span className="text-xs font-medium text-[var(--text-secondary)]">图表</span>
        <button
          onClick={() => setExpanded((value) => !value)}
          className="rounded-md p-1 hover:bg-[var(--bg-hover)] text-[var(--text-tertiary)] transition-colors"
          title={expanded ? '收起图表' : '展开图表'}
        >
          {expanded ? <Minimize2 size={14} /> : <Maximize2 size={14} />}
        </button>
      </div>
      {state === 'loading' && (
        <p role="status" className="px-4 py-2 text-xs text-[var(--text-tertiary)]">
          正在绘制图表
        </p>
      )}
      {state === 'error' && (
        <p role="status" className="px-4 py-2 text-xs text-[var(--color-error)]">
          图表绘制失败：{error}。可查看原始数据。
        </p>
      )}
      <div
        ref={containerRef}
        role="group"
        aria-label="图表"
        className="max-w-full overflow-auto p-2"
        style={{ maxHeight: expanded ? undefined : 420 }}
      />
      <details className="border-t border-[var(--border-secondary)] px-4 py-2 text-xs text-[var(--text-tertiary)]">
        <summary className="cursor-pointer">查看图表数据</summary>
        <pre className="mt-2 max-h-64 overflow-auto whitespace-pre-wrap break-words">
          {JSON.stringify(spec, null, 2)}
        </pre>
      </details>
    </div>
  );
}
