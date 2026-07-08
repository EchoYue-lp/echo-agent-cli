import { useEffect, useRef, useState } from 'react';
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

function buildChartHtml(spec: unknown): string {
  // Escape `<`, `>`, `&` as JSON unicode escapes so a user-controlled spec
  // containing `</script>` cannot terminate the inline script element early
  // (N-P1-14). JSON.parse decodes them back, so the rendered spec is unchanged.
  const specJson = JSON.stringify(spec)
    .replace(/</g, '\\u003c')
    .replace(/>/g, '\\u003e')
    .replace(/&/g, '\\u0026');
  // Store spec in a <script type="application/json"> element to avoid
  // </script> injection from user-controlled Vega-Lite spec data.
  return `<!DOCTYPE html>
<html>
<head>
  <script src="https://cdn.jsdelivr.net/npm/vega@5"></script>
  <script src="https://cdn.jsdelivr.net/npm/vega-lite@5"></script>
  <script src="https://cdn.jsdelivr.net/npm/vega-embed@6"></script>
  <style>
    body { margin: 0; display: flex; justify-content: center; background: transparent; }
    #vis { width: 100%; }
  </style>
</head>
<body>
  <div id="vis"></div>
  <script id="vega-spec" type="application/json">${specJson}</script>
  <script>
    var specEl = document.getElementById('vega-spec');
    var spec = JSON.parse(specEl.textContent || '{}');
    vegaEmbed('#vis', spec, {
      actions: { export: true, source: false, compiled: false, editor: false }
    }).catch(function(err) {
      // err.message 可能含用户可控数据(spec 内容回显)。虽然此 iframe 已是
      // sandboxed null-origin (无 allow-same-origin, 爆炸半径为零), 仍用
      // textContent 而非 innerHTML 让纵深防御一致 — 不依赖单一沙箱兜底。
      document.body.innerHTML = '';
      var p = document.createElement('p');
      p.style.cssText = 'color:red;padding:1rem';
      p.textContent = 'Chart render error: ' + (err && err.message ? err.message : String(err));
      document.body.appendChild(p);
    });
  </script>
</body>
</html>`;
}

export function ChartCard({ spec }: ChartCardProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const [expanded, setExpanded] = useState(false);

  useEffect(() => {
    if (!containerRef.current || !spec) return;

    const container = containerRef.current;
    container.innerHTML = '';

    const iframe = document.createElement('iframe');
    iframe.style.width = '100%';
    iframe.style.height = expanded ? '600px' : '400px';
    iframe.style.border = 'none';
    // Sandbox with allow-scripts only: omit allow-same-origin so a malicious
    // spec cannot reach out to the parent document's origin (N-P1-14).
    iframe.sandbox.add('allow-scripts');
    iframe.srcdoc = buildChartHtml(spec);
    container.appendChild(iframe);
  }, [spec, expanded]);

  return (
    <div
      className={`rounded-lg border border-[var(--border-primary)] bg-[var(--bg-primary)] overflow-hidden`}
    >
      <div className="flex items-center justify-between px-4 py-2 border-b border-[var(--border-primary)] bg-[var(--bg-secondary)]">
        <span className="text-xs font-medium text-[var(--text-secondary)]">Chart</span>
        <button
          onClick={() => setExpanded(!expanded)}
          className="p-1 rounded-md hover:bg-[var(--bg-hover)] text-[var(--text-tertiary)] transition-colors"
          title={expanded ? 'Collapse' : 'Expand'}
        >
          {expanded ? <Minimize2 size={14} /> : <Maximize2 size={14} />}
        </button>
      </div>
      <div
        ref={containerRef}
        className="p-2 flex justify-center"
        style={{ minHeight: expanded ? '600px' : '400px' }}
      />
    </div>
  );
}
