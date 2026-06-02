import { useEffect, useState } from 'react';
import { memoryApi } from '../../api/endpoints';
import type { MemoryEntry } from '../../types/api';
import { Brain, Plus, Search, Trash2 } from 'lucide-react';

export function MemoryPanel() {
  const [entries, setEntries] = useState<MemoryEntry[]>([]);
  const [namespaces, setNamespaces] = useState<string[]>([]);
  const [selectedNs, setSelectedNs] = useState<string>('');
  const [searchQuery, setSearchQuery] = useState('');
  const [showAdd, setShowAdd] = useState(false);
  const [addForm, setAddForm] = useState({ namespace: 'default', key: '', value: '' });

  useEffect(() => {
    memoryApi
      .namespaces()
      .then((data) => setNamespaces(data.namespaces.map((ns) => ns.join('/'))))
      .catch(console.error);
    loadEntries();
  }, []);

  const loadEntries = async (ns?: string) => {
    try {
      const data = await memoryApi.list(ns || undefined);
      setEntries(data);
    } catch (e) {
      console.error(e);
    }
  };

  const search = async () => {
    if (!searchQuery.trim()) {
      loadEntries(selectedNs || undefined);
      return;
    }
    try {
      const data = await memoryApi.search(searchQuery, selectedNs || undefined);
      setEntries(data);
    } catch (e) {
      console.error(e);
    }
  };

  const add = async () => {
    try {
      let parsedValue: any = addForm.value;
      try {
        parsedValue = JSON.parse(addForm.value);
      } catch {
        /* keep as string */
      }

      await memoryApi.add({
        namespace: addForm.namespace,
        key: addForm.key,
        value: parsedValue,
      });
      setShowAdd(false);
      setAddForm({ namespace: 'default', key: '', value: '' });
      memoryApi
        .namespaces()
        .then((data) => setNamespaces(data.namespaces.map((ns) => ns.join('/'))))
        .catch(console.error);
      loadEntries(selectedNs || undefined);
    } catch (e) {
      console.error(e);
    }
  };

  const remove = async (namespace: string, key: string) => {
    try {
      await memoryApi.delete({ namespace, key });
      loadEntries(selectedNs || undefined);
    } catch (e) {
      console.error(e);
    }
  };

  const s = {
    text: 'var(--text-primary)',
    textSec: 'var(--text-secondary)',
    textTer: 'var(--text-tertiary)',
    border: 'var(--border-primary)',
    bg: 'var(--bg-primary)',
    bgHover: 'var(--bg-hover)',
    bgInput: 'var(--bg-input)',
    accent: 'var(--accent)',
  };

  return (
    <div className="p-3 space-y-3">
      <div className="flex items-center justify-between">
        <h3 className="text-sm font-semibold" style={{ color: s.text }}>
          记忆 ({entries.length})
        </h3>
        <button
          onClick={() => setShowAdd(!showAdd)}
          className="rounded p-1 transition-colors"
          style={{ color: s.textTer }}
        >
          <Plus size={16} />
        </button>
      </div>

      <select
        value={selectedNs}
        onChange={(e) => {
          setSelectedNs(e.target.value);
          loadEntries(e.target.value || undefined);
        }}
        className="w-full rounded-lg border px-2 py-1.5 text-xs"
        style={{ background: s.bgInput, borderColor: s.border, color: s.text }}
      >
        <option value="">所有命名空间</option>
        {namespaces.map((ns) => (
          <option key={ns} value={ns}>
            {ns}
          </option>
        ))}
      </select>

      <div className="flex gap-2">
        <input
          value={searchQuery}
          onChange={(e) => setSearchQuery(e.target.value)}
          onKeyDown={(e) => e.key === 'Enter' && search()}
          className="flex-1 rounded-lg border px-2 py-1.5 text-xs"
          style={{ background: s.bgInput, borderColor: s.border, color: s.text }}
          placeholder="搜索..."
        />
        <button
          onClick={search}
          className="rounded-lg px-2 py-1 transition-colors"
          style={{ background: s.bgHover, color: s.textSec }}
        >
          <Search size={14} />
        </button>
      </div>

      {showAdd && (
        <div
          className="space-y-2 rounded-lg border p-3"
          style={{ borderColor: s.border, background: s.bgHover }}
        >
          <input
            value={addForm.namespace}
            onChange={(e) => setAddForm({ ...addForm, namespace: e.target.value })}
            className="w-full rounded-lg border px-2 py-1.5 text-xs"
            style={{ background: s.bgInput, borderColor: s.border, color: s.text }}
            placeholder="命名空间"
          />
          <input
            value={addForm.key}
            onChange={(e) => setAddForm({ ...addForm, key: e.target.value })}
            className="w-full rounded-lg border px-2 py-1.5 text-xs"
            style={{ background: s.bgInput, borderColor: s.border, color: s.text }}
            placeholder="键"
          />
          <textarea
            value={addForm.value}
            onChange={(e) => setAddForm({ ...addForm, value: e.target.value })}
            className="w-full rounded-lg border px-2 py-1.5 text-xs"
            style={{ background: s.bgInput, borderColor: s.border, color: s.text }}
            rows={3}
            placeholder="值"
          />
          <button
            onClick={add}
            className="w-full rounded-lg py-1.5 text-xs font-medium text-white"
            style={{ background: s.accent }}
          >
            添加
          </button>
        </div>
      )}

      {entries.map((e) => (
        <div
          key={`${e.namespace}/${e.key}`}
          className="rounded-lg border px-3 py-2"
          style={{ borderColor: s.border, background: s.bg }}
        >
          <div className="flex items-center justify-between">
            <div className="flex items-center gap-2">
              <Brain size={12} style={{ color: 'var(--color-purple)' }} />
              <span className="text-xs font-medium" style={{ color: s.text }}>
                {e.key}
              </span>
              <span
                className="rounded-full px-1.5 py-0.5 text-[10px]"
                style={{ background: 'var(--color-purple-bg)', color: 'var(--color-purple)' }}
              >
                {e.namespace}
              </span>
            </div>
            <button onClick={() => remove(e.namespace, e.key)} style={{ color: s.textTer }}>
              <Trash2 size={12} />
            </button>
          </div>
          <p className="mt-1 text-xs break-all" style={{ color: s.textSec }}>
            {typeof e.value === 'string' ? e.value : JSON.stringify(e.value, null, 2)}
          </p>
        </div>
      ))}
    </div>
  );
}
