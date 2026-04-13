import { useEffect, useState } from 'react';
import { permissionsApi } from '../../api/endpoints';
import type { PermissionRule } from '../../types/api';
import { Lock, Plus, Trash2 } from 'lucide-react';

export function PermissionsPanel() {
  const [mode, setMode] = useState<string>('auto');
  const [rules, setRules] = useState<PermissionRule[]>([]);
  const [showAdd, setShowAdd] = useState(false);
  const [addForm, setAddForm] = useState({ name: '', tool_pattern: '', effect: 'ask' as 'allow' | 'deny' | 'ask' });

  useEffect(() => {
    permissionsApi.getMode().then((m) => setMode(m.mode)).catch(console.error);
    permissionsApi.listRules().then(setRules).catch(console.error);
  }, []);

  const changeMode = async (m: string) => {
    try {
      await permissionsApi.setMode(m);
      setMode(m);
    } catch (e) {
      console.error(e);
    }
  };

  const addRule = async () => {
    try {
      await permissionsApi.addRule(addForm);
      setShowAdd(false);
      setAddForm({ name: '', tool_pattern: '', effect: 'ask' });
      permissionsApi.listRules().then(setRules);
    } catch (e) {
      console.error(e);
    }
  };

  const removeRule = async (name: string) => {
    try {
      await permissionsApi.removeRule(name);
      permissionsApi.listRules().then(setRules);
    } catch (e) {
      console.error(e);
    }
  };

  const modes = [
    { value: 'auto', label: 'Auto', desc: 'Auto-approve all' },
    { value: 'ask', label: 'Ask', desc: 'Ask for unknown tools' },
    { value: 'strict', label: 'Strict', desc: 'Deny unknown tools' },
  ];

  return (
    <div className="p-3 space-y-3">
      <h3 className="text-sm font-semibold text-gray-700">Permission Mode</h3>

      {/* Mode selector */}
      <div className="space-y-1">
        {modes.map((m) => (
          <label
            key={m.value}
            className={`flex items-center gap-2 rounded border px-3 py-2 cursor-pointer transition ${
              mode === m.value ? 'border-indigo-300 bg-indigo-50' : 'border-gray-200 hover:bg-gray-50'
            }`}
          >
            <input
              type="radio"
              name="mode"
              value={m.value}
              checked={mode === m.value}
              onChange={() => changeMode(m.value)}
              className="accent-indigo-600"
            />
            <div>
              <span className="text-sm font-medium">{m.label}</span>
              <span className="ml-2 text-xs text-gray-400">{m.desc}</span>
            </div>
          </label>
        ))}
      </div>

      {/* Rules */}
      <div className="flex items-center justify-between">
        <h3 className="text-sm font-semibold text-gray-700">Rules ({rules.length})</h3>
        <button onClick={() => setShowAdd(!showAdd)} className="rounded p-1 hover:bg-gray-100">
          <Plus size={16} />
        </button>
      </div>

      {showAdd && (
        <div className="space-y-2 rounded border bg-gray-50 p-3">
          <input
            value={addForm.name}
            onChange={(e) => setAddForm({ ...addForm, name: e.target.value })}
            className="w-full rounded border px-2 py-1 text-sm"
            placeholder="Rule name"
          />
          <input
            value={addForm.tool_pattern}
            onChange={(e) => setAddForm({ ...addForm, tool_pattern: e.target.value })}
            className="w-full rounded border px-2 py-1 text-sm"
            placeholder="Tool pattern (e.g. Bash(rm:*))"
          />
          <select
            value={addForm.effect}
            onChange={(e) => setAddForm({ ...addForm, effect: e.target.value as 'allow' | 'deny' | 'ask' })}
            className="w-full rounded border px-2 py-1 text-sm"
          >
            <option value="allow">Allow</option>
            <option value="deny">Deny</option>
            <option value="ask">Ask</option>
          </select>
          <button onClick={addRule} className="w-full rounded bg-indigo-600 py-1.5 text-sm text-white">
            Add Rule
          </button>
        </div>
      )}

      {rules.map((rule) => (
        <div key={rule.name} className="flex items-center gap-2 rounded border border-gray-200 bg-white px-3 py-2">
          <Lock size={12} className="text-gray-400" />
          <span className="text-xs font-mono">{rule.tool_pattern}</span>
          <span className={`rounded px-1.5 py-0.5 text-[10px] ${
            rule.effect === 'allow' ? 'bg-green-50 text-green-600' :
            rule.effect === 'deny' ? 'bg-red-50 text-red-600' :
            'bg-amber-50 text-amber-600'
          }`}>
            {rule.effect}
          </span>
          <button onClick={() => removeRule(rule.name)} className="ml-auto text-gray-400 hover:text-red-500">
            <Trash2 size={12} />
          </button>
        </div>
      ))}
    </div>
  );
}
