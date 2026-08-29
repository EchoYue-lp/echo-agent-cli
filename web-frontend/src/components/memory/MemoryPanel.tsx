import { useCallback, useEffect, useRef, useState } from 'react';
import {
  autoMemoryApi,
  memoryApi,
  type AutoMemoryObservation,
  type AutoMemoryStatus,
} from '../../api/endpoints';
import type { MemoryEntry } from '../../types/api';
import { Brain, Loader2, Plus, Search, Sparkles, Trash2 } from 'lucide-react';
import { workspaceIdForView } from '../../lib/viewAddress';
import { useConversationStore } from '../../stores/conversationStore';
import { useWorkspaceStore } from '../../stores/workspaceStore';

const AGENT_MEMORY_NAMESPACE = 'agent/memories';

function isCurrentWorkspace(workspaceId: string): boolean {
  return workspaceIdForView(useWorkspaceStore.getState().current?.id) === workspaceId;
}

export function MemoryPanel() {
  const workspaceId = useWorkspaceStore((state) => workspaceIdForView(state.current?.id));
  const conversationId = useConversationStore((state) => state.activeId);
  const scopeRef = useRef({ workspaceId, generation: 0 });
  if (scopeRef.current.workspaceId !== workspaceId) {
    scopeRef.current = {
      workspaceId,
      generation: scopeRef.current.generation + 1,
    };
  }
  const scopeGeneration = scopeRef.current.generation;
  const entriesRequestRef = useRef(0);
  const autoStatusRequestRef = useRef(0);
  const autoActionRequestRef = useRef(0);
  const memoryMutationRequestRef = useRef(0);
  const reflectionRequestRef = useRef(0);
  const [entries, setEntries] = useState<MemoryEntry[]>([]);
  const [entriesWorkspaceId, setEntriesWorkspaceId] = useState('');
  const [entriesScopeGeneration, setEntriesScopeGeneration] = useState(-1);
  const [namespaces, setNamespaces] = useState<string[]>([]);
  const [selectedNs, setSelectedNs] = useState<string>('');
  const [searchQuery, setSearchQuery] = useState('');
  const [showAdd, setShowAdd] = useState(false);
  const [addForm, setAddForm] = useState({
    namespace: AGENT_MEMORY_NAMESPACE,
    key: '',
    value: '',
  });
  const [autoStatus, setAutoStatus] = useState<AutoMemoryStatus | null>(null);
  const [autoPreview, setAutoPreview] = useState<AutoMemoryObservation[]>([]);
  const [autoWorkspaceId, setAutoWorkspaceId] = useState('');
  const [autoStatusScopeGeneration, setAutoStatusScopeGeneration] = useState(-1);
  const [autoPreviewScopeGeneration, setAutoPreviewScopeGeneration] = useState(-1);
  const [autoBusy, setAutoBusy] = useState(false);
  const [autoBusyScopeGeneration, setAutoBusyScopeGeneration] = useState(-1);
  const [autoMessage, setAutoMessage] = useState<string | null>(null);
  const [reflectionBusy, setReflectionBusy] = useState(false);
  const [reflectionMessage, setReflectionMessage] = useState<string | null>(null);
  const visibleEntries =
    entriesWorkspaceId === workspaceId && entriesScopeGeneration === scopeGeneration ? entries : [];
  const visibleAutoStatus =
    autoWorkspaceId === workspaceId && autoStatusScopeGeneration === scopeGeneration
      ? autoStatus
      : null;
  const visibleAutoPreview =
    autoWorkspaceId === workspaceId && autoPreviewScopeGeneration === scopeGeneration
      ? autoPreview
      : [];
  const visibleAutoMessage =
    autoWorkspaceId === workspaceId && autoPreviewScopeGeneration === scopeGeneration
      ? autoMessage
      : null;
  const visibleAutoBusy = autoBusyScopeGeneration === scopeGeneration ? autoBusy : false;

  const requestIsCurrent = useCallback(
    (requestWorkspaceId: string, requestGeneration: number) =>
      scopeRef.current.workspaceId === requestWorkspaceId &&
      scopeRef.current.generation === requestGeneration &&
      isCurrentWorkspace(requestWorkspaceId),
    []
  );

  const loadAutoMemoryStatus = useCallback(async () => {
    const requestGeneration = scopeRef.current.generation;
    const requestToken = autoStatusRequestRef.current + 1;
    autoStatusRequestRef.current = requestToken;
    try {
      const status = await autoMemoryApi.status(workspaceId);
      if (
        !requestIsCurrent(workspaceId, requestGeneration) ||
        autoStatusRequestRef.current !== requestToken
      )
        return;
      setAutoWorkspaceId(workspaceId);
      setAutoStatusScopeGeneration(requestGeneration);
      setAutoStatus(status);
    } catch (e) {
      if (
        requestIsCurrent(workspaceId, requestGeneration) &&
        autoStatusRequestRef.current === requestToken
      ) {
        setAutoWorkspaceId(workspaceId);
        setAutoStatusScopeGeneration(requestGeneration);
        setAutoStatus(null);
      }
      console.error(e);
    }
  }, [requestIsCurrent, workspaceId]);

  const toggleAutoMemory = async () => {
    if (!visibleAutoStatus || visibleAutoBusy) return;
    const requestGeneration = scopeRef.current.generation;
    const requestToken = autoActionRequestRef.current + 1;
    autoActionRequestRef.current = requestToken;
    setAutoBusyScopeGeneration(requestGeneration);
    setAutoBusy(true);
    setAutoMessage(null);
    try {
      const status = await autoMemoryApi.toggle(workspaceId, !visibleAutoStatus.enabled);
      if (
        !requestIsCurrent(workspaceId, requestGeneration) ||
        autoActionRequestRef.current !== requestToken
      )
        return;
      setAutoWorkspaceId(workspaceId);
      setAutoStatusScopeGeneration(requestGeneration);
      setAutoStatus(status);
      if (!status.enabled) setAutoPreview([]);
    } catch (e) {
      if (
        requestIsCurrent(workspaceId, requestGeneration) &&
        autoActionRequestRef.current === requestToken
      ) {
        setAutoWorkspaceId(workspaceId);
        setAutoPreviewScopeGeneration(requestGeneration);
        setAutoMessage(e instanceof Error ? e.message : 'Auto Memory 更新失败');
      }
    } finally {
      if (
        requestIsCurrent(workspaceId, requestGeneration) &&
        autoActionRequestRef.current === requestToken
      ) {
        setAutoBusy(false);
      }
    }
  };

  const previewAutoMemory = async () => {
    const requestGeneration = scopeRef.current.generation;
    const requestToken = autoActionRequestRef.current + 1;
    autoActionRequestRef.current = requestToken;
    setAutoBusyScopeGeneration(requestGeneration);
    setAutoBusy(true);
    setAutoMessage(null);
    try {
      const preview = await autoMemoryApi.preview(workspaceId);
      if (
        !requestIsCurrent(workspaceId, requestGeneration) ||
        autoActionRequestRef.current !== requestToken
      )
        return;
      setAutoWorkspaceId(workspaceId);
      setAutoPreviewScopeGeneration(requestGeneration);
      setAutoPreview(preview.observations);
      setAutoMessage(preview.count === 0 ? '当前会话没有可提取观察' : null);
      await loadAutoMemoryStatus();
    } catch (e) {
      if (
        requestIsCurrent(workspaceId, requestGeneration) &&
        autoActionRequestRef.current === requestToken
      ) {
        setAutoWorkspaceId(workspaceId);
        setAutoPreviewScopeGeneration(requestGeneration);
        setAutoMessage(e instanceof Error ? e.message : 'Auto Memory 预览失败');
      }
    } finally {
      if (
        requestIsCurrent(workspaceId, requestGeneration) &&
        autoActionRequestRef.current === requestToken
      ) {
        setAutoBusy(false);
      }
    }
  };

  const extractAutoMemory = async () => {
    const requestGeneration = scopeRef.current.generation;
    const requestToken = autoActionRequestRef.current + 1;
    autoActionRequestRef.current = requestToken;
    setAutoBusyScopeGeneration(requestGeneration);
    setAutoBusy(true);
    setAutoMessage(null);
    try {
      const result = await autoMemoryApi.extract(workspaceId);
      if (
        !requestIsCurrent(workspaceId, requestGeneration) ||
        autoActionRequestRef.current !== requestToken
      )
        return;
      setAutoWorkspaceId(workspaceId);
      setAutoPreviewScopeGeneration(requestGeneration);
      setAutoPreview(result.observations);
      setAutoMessage(
        result.success
          ? `已将 ${result.queued ?? result.count} 条候选送入 Review Inbox`
          : result.message || 'Auto Memory 未生成候选'
      );
      await loadAutoMemoryStatus();
    } catch (e) {
      if (
        requestIsCurrent(workspaceId, requestGeneration) &&
        autoActionRequestRef.current === requestToken
      ) {
        setAutoWorkspaceId(workspaceId);
        setAutoPreviewScopeGeneration(requestGeneration);
        setAutoMessage(e instanceof Error ? e.message : 'Auto Memory 提取失败');
      }
    } finally {
      if (
        requestIsCurrent(workspaceId, requestGeneration) &&
        autoActionRequestRef.current === requestToken
      ) {
        setAutoBusy(false);
      }
    }
  };

  const loadEntries = useCallback(
    async (ns?: string) => {
      const requestGeneration = scopeRef.current.generation;
      const requestToken = entriesRequestRef.current + 1;
      entriesRequestRef.current = requestToken;
      try {
        const data = await memoryApi.list(workspaceId, ns || undefined);
        if (
          !requestIsCurrent(workspaceId, requestGeneration) ||
          entriesRequestRef.current !== requestToken
        )
          return;
        setEntriesWorkspaceId(workspaceId);
        setEntriesScopeGeneration(requestGeneration);
        setEntries(data);
      } catch (e) {
        if (
          requestIsCurrent(workspaceId, requestGeneration) &&
          entriesRequestRef.current === requestToken
        ) {
          setEntriesWorkspaceId(workspaceId);
          setEntriesScopeGeneration(requestGeneration);
          setEntries([]);
        }
        console.error(e);
      }
    },
    [requestIsCurrent, workspaceId]
  );

  useEffect(() => {
    memoryApi
      .namespaces()
      .then((data) => setNamespaces(data.namespaces.map((ns) => ns.join('/'))))
      .catch(console.error);
    void loadEntries();
    void loadAutoMemoryStatus();
  }, [loadAutoMemoryStatus, loadEntries]);

  const search = async () => {
    if (!searchQuery.trim()) {
      loadEntries(selectedNs || undefined);
      return;
    }
    const requestGeneration = scopeRef.current.generation;
    const requestToken = entriesRequestRef.current + 1;
    entriesRequestRef.current = requestToken;
    try {
      const data = await memoryApi.search(workspaceId, searchQuery, selectedNs || undefined);
      if (
        !requestIsCurrent(workspaceId, requestGeneration) ||
        entriesRequestRef.current !== requestToken
      )
        return;
      setEntriesWorkspaceId(workspaceId);
      setEntriesScopeGeneration(requestGeneration);
      setEntries(data);
    } catch (e) {
      if (
        requestIsCurrent(workspaceId, requestGeneration) &&
        entriesRequestRef.current === requestToken
      ) {
        setEntriesWorkspaceId(workspaceId);
        setEntriesScopeGeneration(requestGeneration);
        setEntries([]);
      }
      console.error(e);
    }
  };

  const add = async () => {
    const requestGeneration = scopeRef.current.generation;
    const requestToken = memoryMutationRequestRef.current + 1;
    memoryMutationRequestRef.current = requestToken;
    try {
      let parsedValue: any = addForm.value;
      try {
        parsedValue = JSON.parse(addForm.value);
      } catch {
        /* keep as string */
      }

      await memoryApi.add(workspaceId, {
        namespace: addForm.namespace,
        key: addForm.key,
        value: parsedValue,
      });
      if (
        !requestIsCurrent(workspaceId, requestGeneration) ||
        memoryMutationRequestRef.current !== requestToken
      )
        return;
      setShowAdd(false);
      setAddForm({ namespace: AGENT_MEMORY_NAMESPACE, key: '', value: '' });
      memoryApi
        .namespaces()
        .then((data) => {
          if (
            requestIsCurrent(workspaceId, requestGeneration) &&
            memoryMutationRequestRef.current === requestToken
          ) {
            setNamespaces(data.namespaces.map((ns) => ns.join('/')));
          }
        })
        .catch(console.error);
      void loadEntries(selectedNs || undefined);
    } catch (e) {
      console.error(e);
    }
  };

  const remove = async (namespace: string, key: string) => {
    const requestGeneration = scopeRef.current.generation;
    const requestToken = memoryMutationRequestRef.current + 1;
    memoryMutationRequestRef.current = requestToken;
    try {
      await memoryApi.delete(workspaceId, { namespace, key });
      if (
        !requestIsCurrent(workspaceId, requestGeneration) ||
        memoryMutationRequestRef.current !== requestToken
      )
        return;
      void loadEntries(selectedNs || undefined);
    } catch (e) {
      console.error(e);
    }
  };

  const reflectSession = async () => {
    if (!conversationId || reflectionBusy) return;
    const requestGeneration = scopeRef.current.generation;
    const requestToken = reflectionRequestRef.current + 1;
    reflectionRequestRef.current = requestToken;
    setReflectionBusy(true);
    setReflectionMessage(null);
    try {
      const receipt = await memoryApi.reflect(workspaceId, conversationId);
      if (
        !requestIsCurrent(workspaceId, requestGeneration) ||
        reflectionRequestRef.current !== requestToken ||
        useConversationStore.getState().activeId !== conversationId
      )
        return;
      setReflectionMessage(`已保存反思 ${receipt.key}: ${receipt.content_summary}`);
      void loadEntries(selectedNs || undefined);
    } catch (e) {
      if (
        requestIsCurrent(workspaceId, requestGeneration) &&
        reflectionRequestRef.current === requestToken &&
        useConversationStore.getState().activeId === conversationId
      ) {
        setReflectionMessage(e instanceof Error ? e.message : '会话反思失败');
      }
    } finally {
      if (
        requestIsCurrent(workspaceId, requestGeneration) &&
        reflectionRequestRef.current === requestToken
      ) {
        setReflectionBusy(false);
      }
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
          记忆 ({visibleEntries.length})
        </h3>
        <button
          onClick={() => setShowAdd(!showAdd)}
          aria-label="添加记忆"
          className="rounded-md p-1 transition-colors"
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
                候选 {visibleAutoStatus?.observation_count ?? 0} · 阈值{' '}
                {Math.round((visibleAutoStatus?.config.min_confidence ?? 0.7) * 100)}%
              </div>
            </div>
          </div>
          <button
            onClick={toggleAutoMemory}
            disabled={!visibleAutoStatus || visibleAutoBusy}
            className="rounded-full px-2 py-1 text-[11px] font-medium transition-colors disabled:opacity-50"
            style={{
              background: visibleAutoStatus?.enabled ? 'var(--color-success-bg)' : s.bgHover,
              color: visibleAutoStatus?.enabled ? 'var(--color-success)' : s.textSec,
            }}
          >
            {visibleAutoStatus?.enabled ? 'ON' : 'OFF'}
          </button>
        </div>

        <div className="mt-3 flex items-center gap-2">
          <button
            onClick={previewAutoMemory}
            disabled={visibleAutoBusy}
            className="flex items-center gap-1 rounded-lg px-2 py-1 text-xs transition-colors disabled:opacity-50"
            style={{ background: s.bgHover, color: s.textSec }}
          >
            {visibleAutoBusy ? (
              <Loader2 size={12} className="animate-spin" />
            ) : (
              <Search size={12} />
            )}
            预览
          </button>
          <button
            onClick={extractAutoMemory}
            disabled={visibleAutoBusy || visibleAutoStatus?.enabled === false}
            className="flex items-center gap-1 rounded-lg px-2 py-1 text-xs font-medium text-white transition-colors disabled:opacity-50"
            style={{ background: s.accent }}
          >
            {visibleAutoBusy ? (
              <Loader2 size={12} className="animate-spin" />
            ) : (
              <Sparkles size={12} />
            )}
            送审
          </button>
          <button
            onClick={reflectSession}
            disabled={reflectionBusy || !conversationId}
            className="flex items-center gap-1 rounded-lg px-2 py-1 text-xs transition-colors disabled:opacity-50"
            style={{ background: s.bgHover, color: s.textSec }}
          >
            {reflectionBusy ? <Loader2 size={12} className="animate-spin" /> : <Brain size={12} />}
            反思
          </button>
        </div>

        {reflectionMessage && (
          <div className="mt-2 text-[11px]" style={{ color: s.textTer }}>
            {reflectionMessage}
          </div>
        )}

        {visibleAutoMessage && (
          <div className="mt-2 text-[11px]" style={{ color: s.textTer }}>
            {visibleAutoMessage}
          </div>
        )}

        {visibleAutoPreview.length > 0 && (
          <div className="mt-3 max-h-40 space-y-2 overflow-auto">
            {visibleAutoPreview.map((obs, idx) => (
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

      {visibleEntries.map((e) => (
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
            <button
              onClick={() => remove(e.namespace, e.key)}
              aria-label={`删除 ${e.key}`}
              style={{ color: s.textTer }}
            >
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
