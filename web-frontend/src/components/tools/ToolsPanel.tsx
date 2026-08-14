import { useEffect, useState } from 'react';
import { toolsApi } from '../../api/endpoints';
import type { ToolInfo } from '../../generated';
import { Wrench, ChevronDown, ChevronRight } from 'lucide-react';

export function ToolsPanel() {
  const [tools, setTools] = useState<ToolInfo[]>([]);
  const [expanded, setExpanded] = useState<string | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);

  const load = async () => {
    setLoadError(null);
    try {
      const data = await toolsApi.list();
      setTools(data);
    } catch (e) {
      console.error('[ToolsPanel] failed to load tools:', e);
      setLoadError(e instanceof Error ? e.message : String(e));
    }
  };

  useEffect(() => {
    load();
  }, []);

  const toggle = async (name: string, enable: boolean) => {
    try {
      if (enable) await toolsApi.enable(name);
      else await toolsApi.disable(name);
      setTools((prev) => prev.map((t) => (t.name === name ? { ...t, enabled: enable } : t)));
    } catch (e) {
      console.error(e);
    }
  };

  return (
    <div className="p-3 space-y-2">
      <div className="flex items-center justify-between mb-2">
        <h3 className="text-sm font-semibold" style={{ color: 'var(--text-primary)' }}>
          工具 ({tools.length})
        </h3>
      </div>
      {loadError && (
        <div
          className="mb-2 rounded-lg px-3 py-2 text-xs"
          style={{ background: 'var(--color-error-bg)', color: 'var(--color-error)' }}
        >
          工具列表加载失败：{loadError}
          <button onClick={load} className="ml-2 underline" style={{ color: 'var(--color-error)' }}>
            重试
          </button>
        </div>
      )}
      {tools.map((tool) => (
        <div
          key={tool.name}
          className="rounded-lg border"
          style={{ borderColor: 'var(--border-primary)', background: 'var(--bg-primary)' }}
        >
          <div className="flex items-center gap-2 px-3 py-2">
            <button
              onClick={() => setExpanded(expanded === tool.name ? null : tool.name)}
              style={{ color: 'var(--text-tertiary)' }}
            >
              {expanded === tool.name ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
            </button>
            <Wrench size={12} style={{ color: 'var(--text-tertiary)' }} />
            <span
              className="flex-1 truncate text-xs font-mono"
              style={{ color: 'var(--text-primary)' }}
            >
              {tool.name}
            </span>
            <button
              onClick={() => toggle(tool.name, !tool.enabled)}
              className={`relative h-5 w-9 rounded-full transition ${tool.enabled ? 'bg-[var(--accent)]' : ''}`}
              style={{ background: tool.enabled ? 'var(--accent)' : 'var(--text-tertiary)' }}
            >
              <span
                className={`absolute top-0.5 h-4 w-4 rounded-full bg-white transition ${tool.enabled ? 'left-[18px]' : 'left-0.5'}`}
              />
            </button>
          </div>
          {expanded === tool.name && (
            <div className="border-t px-3 py-2" style={{ borderColor: 'var(--border-primary)' }}>
              <p className="text-xs" style={{ color: 'var(--text-secondary)' }}>
                {tool.description}
              </p>
              {tool.parameters && (
                <pre
                  className="mt-2 max-h-40 overflow-auto rounded-lg p-2 text-[10px] leading-relaxed"
                  style={{ background: 'var(--bg-code)', color: 'var(--color-code-text)' }}
                >
                  {JSON.stringify(tool.parameters, null, 2)}
                </pre>
              )}
            </div>
          )}
        </div>
      ))}
    </div>
  );
}
