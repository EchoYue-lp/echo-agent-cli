import { useEffect, useState } from 'react';
import { toolsApi } from '../../api/endpoints';
import type { ToolInfo } from '../../types/api';
import { Wrench, ChevronDown, ChevronRight } from 'lucide-react';

export function ToolsPanel() {
  const [tools, setTools] = useState<ToolInfo[]>([]);
  const [expanded, setExpanded] = useState<string | null>(null);

  useEffect(() => {
    toolsApi.list().then(setTools).catch(console.error);
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
        <h3 className="text-sm font-semibold text-gray-700">Tools ({tools.length})</h3>
      </div>
      {tools.map((tool) => (
        <div key={tool.name} className="rounded border border-gray-200 bg-white">
          <div className="flex items-center gap-2 px-3 py-2">
            <button onClick={() => setExpanded(expanded === tool.name ? null : tool.name)}>
              {expanded === tool.name ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
            </button>
            <Wrench size={12} className="text-gray-400" />
            <span className="flex-1 truncate text-xs font-mono">{tool.name}</span>
            <button
              onClick={() => toggle(tool.name, !tool.enabled)}
              className={`relative h-5 w-9 rounded-full transition ${tool.enabled ? 'bg-indigo-500' : 'bg-gray-300'}`}
            >
              <span className={`absolute top-0.5 h-4 w-4 rounded-full bg-white transition ${tool.enabled ? 'left-[18px]' : 'left-0.5'}`} />
            </button>
          </div>
          {expanded === tool.name && (
            <div className="border-t px-3 py-2">
              <p className="text-xs text-gray-500">{tool.description}</p>
              {tool.input_schema && (
                <pre className="mt-2 max-h-40 overflow-auto rounded bg-gray-50 p-2 text-[10px]">
                  {JSON.stringify(tool.input_schema, null, 2)}
                </pre>
              )}
            </div>
          )}
        </div>
      ))}
    </div>
  );
}
