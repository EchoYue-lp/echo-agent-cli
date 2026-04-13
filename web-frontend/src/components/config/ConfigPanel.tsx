import { useEffect, useState } from 'react';
import { configApi } from '../../api/endpoints';
import type { ConfigInfo } from '../../types/api';

export function ConfigPanel() {
  const [config, setConfig] = useState<ConfigInfo | null>(null);
  const [edit, setEdit] = useState<Partial<ConfigInfo>>({});
  const [dirty, setDirty] = useState(false);

  useEffect(() => {
    configApi.get().then((c) => { setConfig(c); setEdit(c); }).catch(console.error);
  }, []);

  const save = async () => {
    try {
      const updated = await configApi.update(edit);
      setConfig(updated);
      setEdit(updated);
      setDirty(false);
    } catch (e) {
      console.error(e);
    }
  };

  if (!config) return <div className="p-3 text-sm text-gray-400">Loading...</div>;

  return (
    <div className="p-3 space-y-3">
      <div className="flex items-center justify-between">
        <h3 className="text-sm font-semibold text-gray-700">Configuration</h3>
        {dirty && (
          <button onClick={save} className="rounded bg-indigo-600 px-3 py-1 text-sm text-white hover:bg-indigo-700">
            Save
          </button>
        )}
      </div>

      <Field label="Model" value={edit.model || ''} onChange={(v) => { setEdit({ ...edit, model: v }); setDirty(true); }} />
      <Field label="System Prompt" value={edit.system_prompt || ''} onChange={(v) => { setEdit({ ...edit, system_prompt: v }); setDirty(true); }} multiline />
      <Field label="Max Iterations" value={String(edit.max_iterations ?? '')} onChange={(v) => { setEdit({ ...edit, max_iterations: Number(v) }); setDirty(true); }} type="number" />
      <Field label="Max Tokens" value={String(edit.max_tokens ?? '')} onChange={(v) => { setEdit({ ...edit, max_tokens: Number(v) }); setDirty(true); }} type="number" />

      <Toggle label="Tools" value={edit.enable_tools ?? false} onChange={(v) => { setEdit({ ...edit, enable_tools: v }); setDirty(true); }} />
      <Toggle label="Memory" value={edit.enable_memory ?? false} onChange={(v) => { setEdit({ ...edit, enable_memory: v }); setDirty(true); }} />
      <Toggle label="Human-in-the-loop" value={edit.enable_human_in_loop ?? false} onChange={(v) => { setEdit({ ...edit, enable_human_in_loop: v }); setDirty(true); }} />
    </div>
  );
}

function Field({ label, value, onChange, multiline, type }: { label: string; value: string; onChange: (v: string) => void; multiline?: boolean; type?: string }) {
  const cls = 'w-full rounded border px-2 py-1 text-sm focus:border-indigo-400 focus:outline-none';
  return (
    <div>
      <label className="mb-1 block text-xs text-gray-500">{label}</label>
      {multiline ? (
        <textarea value={value} onChange={(e) => onChange(e.target.value)} className={cls} rows={3} />
      ) : (
        <input type={type} value={value} onChange={(e) => onChange(e.target.value)} className={cls} />
      )}
    </div>
  );
}

function Toggle({ label, value, onChange }: { label: string; value: boolean; onChange: (v: boolean) => void }) {
  return (
    <div className="flex items-center justify-between">
      <span className="text-sm text-gray-700">{label}</span>
      <button
        onClick={() => onChange(!value)}
        className={`relative h-5 w-9 rounded-full transition ${value ? 'bg-indigo-500' : 'bg-gray-300'}`}
      >
        <span className={`absolute top-0.5 h-4 w-4 rounded-full bg-white transition ${value ? 'left-[18px]' : 'left-0.5'}`} />
      </button>
    </div>
  );
}
