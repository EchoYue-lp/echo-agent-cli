import { useEffect, useState } from 'react';
import {
  autoMemoryApi,
  memoryApi,
  type AutoMemoryObservation,
  type AutoMemoryStatus,
} from '../../api/endpoints';
import type { MemoryEntry } from '../../types/api';
import { Brain, Loader2, Plus, Search, Sparkles, Trash2 } from 'lucide-react';

export function MemoryPanel() {
  const [entries, setEntries] = useState<MemoryEntry[]>([]);
  const [namespaces, setNamespaces] = useState<string[]>([]);
  const [selectedNs, setSelectedNs] = useState<string>('');
  const [searchQuery, setSearchQuery] = useState('');
  const [showAdd, setShowAdd] = useState(false);
  const [addForm, setAddForm] = useState({ namespace: 'default', key: '', value: '' });
  const [autoStatus, setAutoStatus] = useState<AutoMemoryStatus | null>(null);
  const [autoPreview, setAutoPreview] = useState<AutoMemoryObservation[]>([]);
  const [autoBusy, setAutoBusy] = useState(false);
  const [autoMessage, setAutoMessage] = useState<string | null>(null);

  useEffect(() => {
    memoryApi
      .namespaces()
      .then((data) => setNamespaces(data.namespaces.map((ns) => ns.join('/'))))
      .catch(console.error);
    loadEntries();
    loadAutoMemoryStatus();
  }, []);

  const loadAutoMemoryStatus = async () => {
    try {
      const status = await autoMemoryApi.status();
      setAutoStatus(status);
    } catch (e) {
      console.error(e);
    }
  };

  const toggleAutoMemory = async () => {
    if (!autoStatus || autoBusy) return;
    setAutoBusy(true);
    setAutoMessage(null);
    try {
      const status = await autoMemoryApi.toggle(!autoStatus.enabled);
      setAutoStatus(status);
      if (!status.enabled) setAutoPreview([]);
    } catch (e) {
      setAutoMessage(e instanceof Error ? e.message : 'Auto Memory 更新失败');
    } finally {
      setAutoBusy(false);
    }
  };

  const previewAutoMemory = async () => {
    setAutoBusy(true);
    setAutoMessage(null);
    try {
      const preview = await autoMemoryApi.preview();
      setAutoPreview(preview.observations);
      setAutoMessage(preview.count === 0 ? '当前会话没有可提取观察' : null);
      await loadAutoMemoryStatus();
    } catch (e) {
      setAutoMessage(e instanceof Error ? e.message : 'Auto Memory 预览失败');
    } finally {
      setAutoBusy(false);
    }
  };

  const extractAutoMemory = async () => {
    setAutoBusy(true);
    setAutoMessage(null);
    try {
      const result = await autoMemoryApi.extract();
      setAutoPreview(result.observations);
      setAutoMessage(
        result.success
          ? `已保存 ${result.count} 条观察`
          : result.message || 'Auto Memory 未保存'
      );
      await loadAutoMemoryStatus();
      await loadEntries(selectedNs || undefined);
    } catch (e) {
      setAutoMessage(e instanceof Error ? e.message : 'Auto Memory 提取失败');
    } finally {
      setAutoBusy(false);
    }
  };

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

      <div className="rounded-lg border p-3" style={{ borderColor: s.border, background: s.bg }}>
        <div className="flex items-center justify-between gap-3">
          <div className="flex min-w-0 items-center gap-2">
            <Sparkles size={14} style={{ color: s.accent }} />
            <div className="min-w-0">
              <div className="text-xs font-medium" style={{ color: s.text }}>
                Auto Memory
              </div>
              <div className="truncate text-[11px]" style={{ color: s.textTer }}>
                候选 {autoStatus?.observation_count ?? 0} · 阈值{' '}
                {Math.round((autoStatus?.config.min_confidence ?? 0.7) * 100)}%
              </div>
            </div>
          </div>
          <button
            onClick={toggleAutoMemory}
            disabled={!autoStatus || autoBusy}
            className="rounded-full px-2 py-1 text-[11px] font-medium transition-colors disabled:opacity-50"
            style={{
              background: autoStatus?.enabled ? 'var(--color-success-bg)' : s.bgHover,
              color: autoStatus?.enabled ? 'var(--color-success)' : s.textSec,
            }}
          >
            {autoStatus?.enabled ? 'ON' : 'OFF'}
          </button>
        </div>

        <div className="mt-3 flex items-center gap-2">
          <button
            onClick={previewAutoMemory}
            disabled={autoBusy}
            className="flex items-center gap-1 rounded-lg px-2 py-1 text-xs transition-colors disabled:opacity-50"
            style={{ background: s.bgHover, color: s.textSec }}
          >
            {autoBusy ? <Loader2 size={12} className="animate-spin" /> : <Search size={12} />}
            预览
          </button>
          <button
            onClick={extractAutoMemory}
            disabled={autoBusy || autoStatus?.enabled === false}
            className="flex items-center gap-1 rounded-lg px-2 py-1 text-xs font-medium text-white transition-colors disabled:opacity-50"
            style={{ background: s.accent }}
          >
            {autoBusy ? <Loader2 size={12} className="animate-spin" /> : <Sparkles size={12} />}
            提取
          </button>
        </div>

        {autoMessage && (
          <div className="mt-2 text-[11px]" style={{ color: s.textTer }}>
            {autoMessage}
          </div>
        )}

        {autoPreview.length > 0 && (
          <div className="mt-3 max-h-40 space-y-2 overflow-auto">
            {autoPreview.map((obs, idx) => (
              <div
                key={`${obs.category}-${obs.source_turn ?? idx}-${idx}`}
                className="rounded-md border px-2 py-1.5"
                style={{ borderColor: s.border, background: s.bgHover }}
              >
                <div className="mb-1 flex items-center justify-between gap-2">
                  <span className="text-[10px] font-medium" style={{ color: s.accent }}>
                    {obs.category}
                  </span>
                  <span className="text-[10px]" style={{ color: s.textTer }}>
                    {Math.round(obs.confidence * 100)}%
                  </span>
                </div>
                <div className="text-[11px]" style={{ color: s.textSec }}>
                  {obs.text}
                </div>
              </div>
            ))}
          </div>
        )}
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
