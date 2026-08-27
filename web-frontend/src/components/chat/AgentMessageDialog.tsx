import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import {
  Check,
  Clock3,
  LoaderCircle,
  MessageSquare,
  RefreshCw,
  Search,
  Send,
  Users,
  X,
  XCircle,
} from 'lucide-react';
import {
  agentApi,
  type AgentDeliveryRecord,
  type AgentDeliveryOutcome,
  type AgentDeliveryPhase,
  type AgentEndpoint,
} from '../../api/endpoints';
import { useConversationStore } from '../../stores/conversationStore';
import { useToastStore } from '../../stores/toastStore';
import { useWorkspaceStore } from '../../stores/workspaceStore';
import { Modal } from '../common/Modal';
import { AgentGroupPanel } from './AgentGroupPanel';

interface AgentMessageDialogProps {
  isOpen: boolean;
  onClose: () => void;
}

const phaseLabels: Record<Exclude<AgentDeliveryPhase, 'turn_settled'>, string> = {
  persisted: '已持久化',
  claimed: '已认领',
  mailbox_accepted: '邮箱已接收',
  drained: '已进入上下文',
};

const outcomeLabels: Record<AgentDeliveryOutcome, string> = {
  completed: '已完成',
  failed: '失败',
  cancelled: '已取消',
  dropped: '已丢弃',
  outcome_unknown: '结果未知',
};

function deliveryLabel(record: AgentDeliveryRecord): string {
  if (record.phase === 'turn_settled') {
    return record.outcome ? outcomeLabels[record.outcome] : outcomeLabels.outcome_unknown;
  }
  return phaseLabels[record.phase];
}

function StatusIcon({ record }: { record: AgentDeliveryRecord }) {
  if (record.phase === 'turn_settled' && record.outcome === 'completed') {
    return <Check size={13} aria-hidden="true" />;
  }
  if (record.phase === 'turn_settled') return <XCircle size={13} aria-hidden="true" />;
  if (record.phase !== 'persisted') {
    return <LoaderCircle size={13} className="animate-spin" aria-hidden="true" />;
  }
  return <Clock3 size={13} aria-hidden="true" />;
}

export function AgentMessageDialog({ isOpen, onClose }: AgentMessageDialogProps) {
  const currentWorkspace = useWorkspaceStore((state) => state.current);
  const currentConversationId = useConversationStore((state) => state.activeId);
  const [endpoints, setEndpoints] = useState<AgentEndpoint[]>([]);
  const [selectedKey, setSelectedKey] = useState<string | null>(null);
  const [records, setRecords] = useState<AgentDeliveryRecord[]>([]);
  const [query, setQuery] = useState('');
  const [text, setText] = useState('');
  const [loading, setLoading] = useState(false);
  const [refreshing, setRefreshing] = useState(false);
  const [sending, setSending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [view, setView] = useState<'messages' | 'groups'>('messages');
  const searchRef = useRef<HTMLInputElement>(null);

  const endpointKey = useCallback(
    (endpoint: AgentEndpoint) =>
      `${endpoint.address.workspace_id}/${endpoint.address.conversation_id}`,
    []
  );
  const selected = endpoints.find((endpoint) => endpointKey(endpoint) === selectedKey) ?? null;

  const filteredEndpoints = useMemo(() => {
    const normalized = query.trim().toLocaleLowerCase();
    return endpoints.filter((endpoint) => {
      const isCurrent =
        endpoint.address.workspace_id === currentWorkspace?.id &&
        endpoint.address.conversation_id === currentConversationId;
      if (isCurrent) return false;
      if (!normalized) return true;
      return [
        endpoint.workspace_name,
        endpoint.conversation_title ?? '',
        endpoint.address.workspace_id,
        endpoint.address.conversation_id,
      ].some((value) => value.toLocaleLowerCase().includes(normalized));
    });
  }, [currentConversationId, currentWorkspace?.id, endpoints, query]);

  const loadRecords = useCallback(async (endpoint: AgentEndpoint, quiet = false) => {
    if (!quiet) setRefreshing(true);
    try {
      const response = await agentApi.status(
        endpoint.address.workspace_id,
        endpoint.address.conversation_id
      );
      setRecords(response.records.slice().reverse().slice(0, 20));
      setError(null);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      if (!quiet) setRefreshing(false);
    }
  }, []);

  useEffect(() => {
    if (!isOpen) return;
    let active = true;
    setLoading(true);
    setError(null);
    agentApi
      .list()
      .then((response) => {
        if (!active) return;
        setEndpoints(response.endpoints);
        setSelectedKey((existing) => {
          if (
            existing &&
            response.endpoints.some((endpoint) => endpointKey(endpoint) === existing)
          ) {
            return existing;
          }
          const firstTarget = response.endpoints.find(
            (endpoint) =>
              endpoint.address.workspace_id !== currentWorkspace?.id ||
              endpoint.address.conversation_id !== currentConversationId
          );
          return firstTarget ? endpointKey(firstTarget) : null;
        });
      })
      .catch((cause) => {
        if (active) setError(cause instanceof Error ? cause.message : String(cause));
      })
      .finally(() => {
        if (active) setLoading(false);
      });
    return () => {
      active = false;
    };
  }, [currentConversationId, currentWorkspace?.id, endpointKey, isOpen]);

  useEffect(() => {
    if (!isOpen || !selected) {
      setRecords([]);
      return;
    }
    setRecords([]);
    void loadRecords(selected);
  }, [isOpen, loadRecords, selected]);

  useEffect(() => {
    if (!isOpen || !selected || !records.some((record) => record.phase !== 'turn_settled')) {
      return;
    }
    const interval = window.setInterval(() => void loadRecords(selected, true), 1200);
    return () => window.clearInterval(interval);
  }, [isOpen, loadRecords, records, selected]);

  if (!isOpen) return null;

  const handleSend = async () => {
    if (sending || !selected || !text.trim()) return;
    setSending(true);
    setError(null);
    try {
      const response = await agentApi.send({
        toWorkspaceId: selected.address.workspace_id,
        toConversationId: selected.address.conversation_id,
        text: text.trim(),
        ...(currentWorkspace && currentConversationId
          ? {
              fromWorkspaceId: currentWorkspace.id,
              fromConversationId: currentConversationId,
            }
          : {}),
      });
      setText('');
      if (response.receipt.durability.status === 'degraded') {
        useToastStore
          .getState()
          .addToast('warning', `消息已接收，但磁盘同步仍待恢复：${response.receipt.message_id}`);
      } else {
        useToastStore.getState().addToast('success', `消息已排队：${response.receipt.message_id}`);
      }
      await loadRecords(selected, true);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setSending(false);
    }
  };

  return (
    <Modal
      onClose={onClose}
      ariaLabel="Agent 消息"
      initialFocusRef={searchRef}
      className="flex h-[min(720px,86vh)] w-[min(960px,calc(100vw-2rem))] flex-col overflow-hidden rounded-lg border border-[var(--border-primary)] bg-[var(--bg-primary)] shadow-[var(--shadow-xl)]"
    >
      <header className="flex h-12 shrink-0 items-center justify-between border-b border-[var(--border-primary)] px-4">
        <div className="flex min-w-0 items-center gap-4">
          <h2 className="shrink-0 text-sm font-semibold text-[var(--text-primary)]">Agent 协作</h2>
          <div
            className="flex h-8 items-center rounded-md bg-[var(--bg-secondary)] p-0.5"
            role="tablist"
          >
            <button
              type="button"
              role="tab"
              aria-selected={view === 'messages'}
              onClick={() => setView('messages')}
              className={`flex h-7 items-center gap-1.5 rounded px-2 text-xs ${
                view === 'messages'
                  ? 'bg-[var(--bg-primary)] text-[var(--text-primary)] shadow-sm'
                  : 'text-[var(--text-tertiary)] hover:text-[var(--text-primary)]'
              }`}
            >
              <MessageSquare size={13} />
              消息
            </button>
            <button
              type="button"
              role="tab"
              aria-selected={view === 'groups'}
              onClick={() => setView('groups')}
              className={`flex h-7 items-center gap-1.5 rounded px-2 text-xs ${
                view === 'groups'
                  ? 'bg-[var(--bg-primary)] text-[var(--text-primary)] shadow-sm'
                  : 'text-[var(--text-tertiary)] hover:text-[var(--text-primary)]'
              }`}
            >
              <Users size={13} />
              Agent 组
            </button>
          </div>
        </div>
        <button
          type="button"
          onClick={onClose}
          className="flex h-8 w-8 items-center justify-center rounded-md text-[var(--text-tertiary)] hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]"
          title="关闭"
          aria-label="关闭 Agent 消息"
        >
          <X size={16} />
        </button>
      </header>

      {view === 'messages' ? (
        <div className="grid min-h-0 flex-1 grid-cols-1 grid-rows-[minmax(160px,0.4fr)_minmax(0,0.6fr)] md:grid-cols-[minmax(230px,0.36fr)_minmax(0,0.64fr)] md:grid-rows-1">
          <aside className="flex min-h-0 flex-col border-b border-[var(--border-primary)] md:border-b-0 md:border-r">
            <div className="relative border-b border-[var(--border-secondary)] p-3">
              <Search
                size={14}
                className="pointer-events-none absolute left-6 top-1/2 -translate-y-1/2 text-[var(--text-tertiary)]"
              />
              <input
                ref={searchRef}
                value={query}
                onChange={(event) => setQuery(event.target.value)}
                className="h-9 w-full rounded-md border border-[var(--border-primary)] bg-[var(--bg-input)] pl-8 pr-3 text-sm text-[var(--text-primary)] outline-none focus:border-[var(--border-focus)]"
                placeholder="搜索会话"
                aria-label="搜索 Agent 会话"
              />
            </div>
            <div className="min-h-32 flex-1 overflow-y-auto py-1">
              {loading ? (
                <div className="flex h-full items-center justify-center text-[var(--text-tertiary)]">
                  <LoaderCircle size={18} className="animate-spin" aria-label="加载中" />
                </div>
              ) : error && endpoints.length === 0 ? (
                <div
                  className="break-words px-4 py-8 text-center text-xs text-[var(--color-error-text)]"
                  role="alert"
                >
                  {error}
                </div>
              ) : filteredEndpoints.length === 0 ? (
                <div className="px-4 py-8 text-center text-xs text-[var(--text-tertiary)]">
                  没有可用会话
                </div>
              ) : (
                filteredEndpoints.map((endpoint) => {
                  const key = endpointKey(endpoint);
                  const active = key === selectedKey;
                  return (
                    <button
                      key={key}
                      type="button"
                      onClick={() => setSelectedKey(key)}
                      className={`w-full border-l-2 px-3 py-2.5 text-left transition-colors ${
                        active
                          ? 'border-[var(--accent)] bg-[var(--bg-sidebar-active)]'
                          : 'border-transparent hover:bg-[var(--bg-hover)]'
                      }`}
                    >
                      <span className="block truncate text-sm font-medium text-[var(--text-primary)]">
                        {endpoint.conversation_title || '未命名会话'}
                      </span>
                      <span className="mt-0.5 block truncate text-[11px] text-[var(--text-tertiary)]">
                        {endpoint.workspace_name} · {endpoint.address.conversation_id}
                      </span>
                    </button>
                  );
                })
              )}
            </div>
          </aside>

          <main className="flex min-h-0 flex-col">
            {selected ? (
              <>
                <div className="flex h-12 shrink-0 items-center justify-between border-b border-[var(--border-secondary)] px-4">
                  <div className="min-w-0">
                    <div className="truncate text-sm font-medium text-[var(--text-primary)]">
                      {selected.conversation_title || '未命名会话'}
                    </div>
                    <div className="truncate text-[11px] text-[var(--text-tertiary)]">
                      {selected.workspace_name} / {selected.address.conversation_id}
                    </div>
                  </div>
                  <button
                    type="button"
                    onClick={() => void loadRecords(selected)}
                    disabled={refreshing}
                    className="flex h-8 w-8 shrink-0 items-center justify-center rounded-md text-[var(--text-tertiary)] hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)] disabled:opacity-50"
                    title="刷新投递状态"
                    aria-label="刷新投递状态"
                  >
                    <RefreshCw size={15} className={refreshing ? 'animate-spin' : ''} />
                  </button>
                </div>

                <div className="min-h-0 flex-1 overflow-y-auto px-4 py-2">
                  {records.length === 0 ? (
                    <div className="flex h-full min-h-28 items-center justify-center text-xs text-[var(--text-tertiary)]">
                      暂无投递记录
                    </div>
                  ) : (
                    <div className="divide-y divide-[var(--border-secondary)]">
                      {records.map((record) => (
                        <div key={record.message_id} className="py-3">
                          <div className="flex min-w-0 items-center gap-2">
                            <span
                              className={`flex shrink-0 items-center gap-1 text-xs ${
                                record.phase === 'turn_settled' && record.outcome !== 'completed'
                                  ? 'text-[var(--color-error-text)]'
                                  : record.phase === 'turn_settled'
                                    ? 'text-[var(--color-success-text)]'
                                    : 'text-[var(--text-secondary)]'
                              }`}
                            >
                              <StatusIcon record={record} />
                              {deliveryLabel(record)}
                            </span>
                            <span className="min-w-0 flex-1 truncate font-mono text-[10px] text-[var(--text-tertiary)]">
                              {record.message_id}
                            </span>
                            <span className="shrink-0 text-[10px] tabular-nums text-[var(--text-tertiary)]">
                              #{record.attempt}
                            </span>
                          </div>
                          {(record.reply_message_id || record.reason) && (
                            <div className="mt-1 truncate text-[11px] text-[var(--text-tertiary)]">
                              {record.reason || `回复 ${record.reply_message_id}`}
                            </div>
                          )}
                        </div>
                      ))}
                    </div>
                  )}
                </div>

                <div className="shrink-0 border-t border-[var(--border-primary)] p-3">
                  {error && (
                    <div className="mb-2 text-xs text-[var(--color-error-text)]" role="alert">
                      {error}
                    </div>
                  )}
                  <div className="flex items-end gap-2">
                    <textarea
                      value={text}
                      onChange={(event) => setText(event.target.value)}
                      onKeyDown={(event) => {
                        if (event.key === 'Enter' && !event.shiftKey) {
                          event.preventDefault();
                          void handleSend();
                        }
                      }}
                      className="min-h-20 max-h-40 flex-1 resize-y rounded-md border border-[var(--border-primary)] bg-[var(--bg-input)] px-3 py-2 text-sm text-[var(--text-primary)] outline-none focus:border-[var(--border-focus)]"
                      placeholder="发送消息"
                      aria-label="Agent 消息内容"
                    />
                    <button
                      type="button"
                      onClick={() => void handleSend()}
                      disabled={sending || !text.trim()}
                      className="flex h-9 items-center gap-2 rounded-md bg-[var(--accent)] px-3 text-sm font-medium text-[var(--text-on-accent)] hover:opacity-90 disabled:cursor-not-allowed disabled:opacity-45"
                    >
                      {sending ? (
                        <LoaderCircle size={15} className="animate-spin" />
                      ) : (
                        <Send size={15} />
                      )}
                      发送
                    </button>
                  </div>
                </div>
              </>
            ) : (
              <div className="flex h-full items-center justify-center text-xs text-[var(--text-tertiary)]">
                选择一个会话
              </div>
            )}
          </main>
        </div>
      ) : (
        <AgentGroupPanel
          endpoints={endpoints}
          currentAddress={
            currentWorkspace && currentConversationId
              ? {
                  workspace_id: currentWorkspace.id,
                  conversation_id: currentConversationId,
                }
              : null
          }
        />
      )}
    </Modal>
  );
}
