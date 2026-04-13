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
    memoryApi.namespaces().then(setNamespaces).catch(console.error);
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
    if (!searchQuery.trim()) { loadEntries(selectedNs || undefined); return; }
    try {
      const data = await memoryApi.search(searchQuery, selectedNs || undefined);
      setEntries(data);
    } catch (e) {
      console.error(e);
    }
  };

  const add = async () => {
    try {
      await memoryApi.add(addForm);
      setShowAdd(false);
      setAddForm({ namespace: 'default', key: '', value: '' });
      memoryApi.namespaces().then(setNamespaces);
      loadEntries(selectedNs || undefined);
    } catch (e) {
      console.error(e);
    }
  };

  const remove = async (id: string) => {
    try {
      await memoryApi.delete(id);
      loadEntries(selectedNs || undefined);
    } catch (e) {
      console.error(e);
    }
  };

  return (
    <div className="p-3 space-y-3">
      <div className="flex items-center justify-between">
        <h3 className="text-sm font-semibold text-gray-700">Memory ({entries.length})</h3>
        <button onClick={() => setShowAdd(!showAdd)} className="rounded p-1 hover:bg-gray-100">
          <Plus size={16} />
        </button>
      </div>

      {/* Namespace filter */}
      <select
        value={selectedNs}
        onChange={(e) => { setSelectedNs(e.target.value); loadEntries(e.target.value || undefined); }}
        className="w-full rounded border px-2 py-1 text-sm"
      >
        <option value="">All namespaces</option>
        {namespaces.map((ns) => <option key={ns} value={ns}>{ns}</option>)}
      </select>

      {/* Search */}
      <div className="flex gap-2">
        <input
          value={searchQuery}
          onChange={(e) => setSearchQuery(e.target.value)}
          onKeyDown={(e) => e.key === 'Enter' && search()}
          className="flex-1 rounded border px-2 py-1 text-sm"
          placeholder="Search..."
        />
        <button onClick={search} className="rounded bg-gray-100 px-2 py-1 hover:bg-gray-200">
          <Search size={14} />
        </button>
      </div>

      {/* Add form */}
      {showAdd && (
        <div className="space-y-2 rounded border bg-gray-50 p-3">
          <input
            value={addForm.namespace}
            onChange={(e) => setAddForm({ ...addForm, namespace: e.target.value })}
            className="w-full rounded border px-2 py-1 text-sm"
            placeholder="Namespace"
          />
          <input
            value={addForm.key}
            onChange={(e) => setAddForm({ ...addForm, key: e.target.value })}
            className="w-full rounded border px-2 py-1 text-sm"
            placeholder="Key"
          />
          <textarea
            value={addForm.value}
            onChange={(e) => setAddForm({ ...addForm, value: e.target.value })}
            className="w-full rounded border px-2 py-1 text-sm"
            rows={3}
            placeholder="Value"
          />
          <button onClick={add} className="w-full rounded bg-indigo-600 py-1.5 text-sm text-white">
            Add
          </button>
        </div>
      )}

      {/* Entries */}
      {entries.map((e) => (
        <div key={e.id} className="rounded border border-gray-200 bg-white px-3 py-2">
          <div className="flex items-center justify-between">
            <div className="flex items-center gap-2">
              <Brain size={12} className="text-purple-500" />
              <span className="text-xs font-medium">{e.key}</span>
              <span className="rounded bg-purple-50 px-1.5 py-0.5 text-[10px] text-purple-600">{e.namespace}</span>
            </div>
            <button onClick={() => remove(e.id)} className="text-gray-400 hover:text-red-500">
              <Trash2 size={12} />
            </button>
          </div>
          <p className="mt-1 text-xs text-gray-500 break-all">{e.value}</p>
        </div>
      ))}
    </div>
  );
}
