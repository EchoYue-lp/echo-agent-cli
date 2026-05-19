import { useEffect, useState } from 'react';
import { permissionsApi } from '../../api/endpoints';
import type { PermissionRule } from '../../types/api';
import { Lock, Plus, Trash2 } from 'lucide-react';
import { StatusBadge } from '../common/StatusBadge';

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
    } catch (e) { console.error(e); }
  };

  const addRule = async () => {
    try {
      await permissionsApi.addRule(addForm);
      setShowAdd(false);
      setAddForm({ name: '', tool_pattern: '', effect: 'ask' });
      permissionsApi.listRules().then(setRules);
    } catch (e) { console.error(e); }
  };

  const removeRule = async (name: string) => {
    try {
      await permissionsApi.removeRule(name);
      permissionsApi.listRules().then(setRules);
    } catch (e) { console.error(e); }
  };

  const modes = [
    { value: 'auto', label: '自动', desc: '自动批准所有' },
    { value: 'ask', label: '询问', desc: '未知工具时询问' },
    { value: 'strict', label: '严格', desc: '拒绝未知工具' },
  ];

  const s = {
    text: 'var(--text-primary)',
    textSec: 'var(--text-secondary)',
    textTer: 'var(--text-tertiary)',
    border: 'var(--border-primary)',
    bg: 'var(--bg-primary)',
    bgInput: 'var(--bg-input)',
    accent: 'var(--accent)',
    accentBg: 'var(--accent-bg)',
  };

  return (
    <div className="p-3 space-y-3">
      <h3 className="text-sm font-semibold" style={{ color: s.text }}>权限模式</h3>

      <div className="space-y-1">
        {modes.map((m) => (
          <label key={m.value} className={`flex items-center gap-2 rounded-lg border px-3 py-2 cursor-pointer transition`}
            style={{
              borderColor: mode === m.value ? s.accent : s.border,
              background: mode === m.value ? s.accentBg : s.bg,
            }}>
            <input type="radio" name="mode" value={m.value} checked={mode === m.value}
              onChange={() => changeMode(m.value)} className="accent-[var(--accent)]" />
            <div>
              <span className="text-sm font-medium" style={{ color: s.text }}>{m.label}</span>
              <span className="ml-2 text-xs" style={{ color: s.textTer }}>{m.desc}</span>
            </div>
          </label>
        ))}
      </div>

      <div className="flex items-center justify-between">
        <h3 className="text-sm font-semibold" style={{ color: s.text }}>规则 ({rules.length})</h3>
        <button onClick={() => setShowAdd(!showAdd)} className="rounded p-1 transition-colors" style={{ color: s.textTer }}>
          <Plus size={16} />
        </button>
      </div>

      {showAdd && (
        <div className="space-y-2 rounded-lg border p-3" style={{ borderColor: s.border, background: s.accentBg }}>
          <input value={addForm.name} onChange={(e) => setAddForm({ ...addForm, name: e.target.value })}
            className="w-full rounded-lg border px-2 py-1.5 text-xs"
            style={{ background: s.bgInput, borderColor: s.border, color: s.text }}
            placeholder="规则名称" />
          <input value={addForm.tool_pattern} onChange={(e) => setAddForm({ ...addForm, tool_pattern: e.target.value })}
            className="w-full rounded-lg border px-2 py-1.5 text-xs"
            style={{ background: s.bgInput, borderColor: s.border, color: s.text }}
            placeholder="工具模式（例如 Bash(rm:*))" />
          <select value={addForm.effect} onChange={(e) => setAddForm({ ...addForm, effect: e.target.value as 'allow' | 'deny' | 'ask' })}
            className="w-full rounded-lg border px-2 py-1.5 text-xs"
            style={{ background: s.bgInput, borderColor: s.border, color: s.text }}>
            <option value="allow">允许</option>
            <option value="deny">拒绝</option>
            <option value="ask">询问</option>
          </select>
          <button onClick={addRule} className="w-full rounded-lg py-1.5 text-xs font-medium text-white"
            style={{ background: s.accent }}>
            添加规则
          </button>
        </div>
      )}

      {rules.map((rule) => (
        <div key={rule.name} className="flex items-center gap-2 rounded-lg border px-3 py-2"
          style={{ borderColor: s.border, background: s.bg }}>
          <Lock size={12} style={{ color: s.textTer }} />
          <span className="text-xs font-mono" style={{ color: s.text }}>{rule.tool_pattern}</span>
          <StatusBadge
            status={rule.effect === 'allow' ? 'success' : rule.effect === 'deny' ? 'error' : 'warning'}
            label={rule.effect === 'allow' ? '允许' : rule.effect === 'deny' ? '拒绝' : '询问'}
            size="sm"
          />
          <button onClick={() => removeRule(rule.name)} className="ml-auto" style={{ color: s.textTer }}>
            <Trash2 size={12} />
          </button>
        </div>
      ))}
    </div>
  );
}
