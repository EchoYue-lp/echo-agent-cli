import { useEffect, useState } from 'react';
import { mcpApi } from '../../api/endpoints';
import type { McpServerInfo } from '../../types/api';
import { Globe, Plus, Trash2, ChevronDown, ChevronRight } from 'lucide-react';

export function McpPanel() {
  const [servers, setServers] = useState<McpServerInfo[]>([]);
  const [showForm, setShowForm] = useState(false);
  const [expanded, setExpanded] = useState<string | null>(null);
  const [form, setForm] = useState({ name: '', command: '', args: '' });

  useEffect(() => {
    mcpApi.list().then(setServers).catch(console.error);
  }, []);

  const connect = async () => {
    try {
      await mcpApi.connect({
        name: form.name,
        transport: { stdio: { command: form.command, args: form.args ? form.args.split(/\s+/) : undefined } },
      });
      setShowForm(false);
      setForm({ name: '', command: '', args: '' });
      mcpApi.list().then(setServers);
    } catch (e) {
      console.error(e);
    }
  };

  const disconnect = async (name: string) => {
    try {
      await mcpApi.disconnect(name);
      mcpApi.list().then(setServers);
    } catch (e) {
      console.error(e);
    }
  };

  return (
    <div className="p-3 space-y-2">
      <div className="flex items-center justify-between mb-2">
        <h3 className="text-sm font-semibold text-gray-700">MCP Servers ({servers.length})</h3>
        <button onClick={() => setShowForm(!showForm)} className="rounded p-1 hover:bg-gray-100">
          <Plus size={16} />
        </button>
      </div>

      {showForm && (
        <div className="space-y-2 rounded border bg-gray-50 p-3">
          <input
            value={form.name}
            onChange={(e) => setForm({ ...form, name: e.target.value })}
            className="w-full rounded border px-2 py-1 text-sm"
            placeholder="Server name"
          />
          <input
            value={form.command}
            onChange={(e) => setForm({ ...form, command: e.target.value })}
            className="w-full rounded border px-2 py-1 text-sm"
            placeholder="Command (e.g. npx, python)"
          />
          <input
            value={form.args}
            onChange={(e) => setForm({ ...form, args: e.target.value })}
            className="w-full rounded border px-2 py-1 text-sm"
            placeholder="Args (space separated)"
          />
          <button onClick={connect} className="w-full rounded bg-indigo-600 py-1.5 text-sm text-white hover:bg-indigo-700">
            Connect
          </button>
        </div>
      )}

      {servers.map((s) => (
        <div key={s.name} className="rounded border border-gray-200 bg-white">
          <div className="flex items-center gap-2 px-3 py-2">
            <button onClick={() => setExpanded(expanded === s.name ? null : s.name)}>
              {expanded === s.name ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
            </button>
            <Globe size={12} className="text-green-500" />
            <span className="flex-1 truncate text-xs font-medium">{s.name}</span>
            <span className="text-[10px] text-gray-400">{s.tool_count} tools</span>
            <button onClick={() => disconnect(s.name)} className="text-gray-400 hover:text-red-500">
              <Trash2 size={12} />
            </button>
          </div>
          {expanded === s.name && s.tools.length > 0 && (
            <div className="border-t px-3 py-2 space-y-1">
              {s.tools.map((t) => (
                <div key={t.name} className="text-xs">
                  <span className="font-mono text-gray-700">{t.name}</span>
                  <span className="ml-2 text-gray-400">{t.description}</span>
                </div>
              ))}
            </div>
          )}
        </div>
      ))}
    </div>
  );
}
