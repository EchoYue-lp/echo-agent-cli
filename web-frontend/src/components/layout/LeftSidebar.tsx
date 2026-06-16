import { useState, useRef, useEffect } from 'react';
import {
  Plus,
  Sun,
  Moon,
  Trash2,
  FolderOpen,
  Terminal,
  Settings,
  Code,
  BarChart3,
  GraduationCap,
  MessageSquare,
  Search,
  X,
  ChevronDown,
  ChevronRight,
  Loader2,
} from 'lucide-react';
import { BrandIcon } from '../common/BrandIcon';
import { useUiStore } from '../../stores/uiStore';
import { useWorkspaceStore } from '../../stores/workspaceStore';
import { useConversationStore } from '../../stores/conversationStore';
import { conversationApi } from '../../api/endpoints';
import type { Workspace } from '../../api/endpoints';
import type { ConversationListItem } from '../../types/api';

const KIND_ICONS: Record<string, { icon: typeof Code; color: string }> = {
  code: { icon: Code, color: '#6366f1' },
  data: { icon: BarChart3, color: '#f59e0b' },
  data_analysis: { icon: BarChart3, color: '#f59e0b' },
  research: { icon: GraduationCap, color: '#10b981' },
  general: { icon: MessageSquare, color: '#6b7280' },
};

const MAX_RECENT_CONVERSATIONS = 5;

export function LeftSidebar({ onNewTask }: { onNewTask: () => void }) {
  const theme = useUiStore((s) => s.theme);
  const toggleTheme = useUiStore((s) => s.toggleTheme);
  const openSettings = useUiStore((s) => s.openSettings);
  const toggleTerminal = useUiStore((s) => s.toggleTerminal);

  const workspaces = useWorkspaceStore((s) => s.workspaces);
  const current = useWorkspaceStore((s) => s.current);
  const switchTo = useWorkspaceStore((s) => s.switchTo);
  const deleteWorkspace = useWorkspaceStore((s) => s.delete);

  const conversations = useConversationStore((s) => s.conversations);
  const activeConvId = useConversationStore((s) => s.activeId);
  const isConvLoading = useConversationStore((s) => s.isLoading);
  const loadConversation = useConversationStore((s) => s.loadConversation);

  const [expandedId, setExpandedId] = useState<string | null>(null);
  const [searchQuery, setSearchQuery] = useState('');
  const [searchResults, setSearchResults] = useState<ConversationListItem[]>([]);
  const [isSearching, setIsSearching] = useState(false);
  const [showAllConvs, setShowAllConvs] = useState(false);
  const searchInputRef = useRef<HTMLInputElement>(null);

  // Debounced search across all conversation content
  useEffect(() => {
    if (!searchQuery.trim()) {
      setSearchResults([]);
      return;
    }
    const controller = new AbortController();
    const timer = setTimeout(async () => {
      if (controller.signal.aborted) return;
      setIsSearching(true);
      try {
        const results = await conversationApi.search(searchQuery.trim());
        if (!controller.signal.aborted) {
          setSearchResults(results);
        }
      } catch (e) {
        if (!controller.signal.aborted) {
          console.error('Search failed:', e);
          setSearchResults([]);
        }
      } finally {
        if (!controller.signal.aborted) {
          setIsSearching(false);
        }
      }
    }, 300);
    return () => {
      clearTimeout(timer);
      controller.abort();
    };
  }, [searchQuery]);

  useEffect(() => {
    setShowAllConvs(false);
  }, [current?.id]);

  // Always filter workspace names; search results come from API when searching
  const filtered = searchQuery.trim()
    ? workspaces.filter((w) => w.name.toLowerCase().includes(searchQuery.toLowerCase()))
    : workspaces;
  const isSearchingContent = searchQuery.trim().length > 0;

  const handleSwitch = async (ws: Workspace) => {
    if (current?.id === ws.id) {
      // Already active — toggle expand
      setExpandedId(expandedId === ws.id ? null : ws.id);
      return;
    }
    try {
      await switchTo(ws.id);
      setExpandedId(ws.id);
    } catch (e) {
      console.error('Switch workspace failed:', e);
    }
  };

  const handleDelete = async (id: string, e: React.MouseEvent) => {
    e.stopPropagation();
    if (!confirm('确定删除此任务？所有数据将被清除。')) return;
    try {
      await deleteWorkspace(id);
      if (expandedId === id) setExpandedId(null);
    } catch (e) {
      console.error('Delete workspace failed:', e);
    }
  };

  const handleOpenFolder = async (e: React.MouseEvent, ws: Workspace) => {
    e.stopPropagation();
    try {
      const { fileSystem } = await import('../../lib/tauri-bridge');
      await fileSystem.openPath(ws.root);
    } catch (err) {
      console.error('Open folder failed:', err);
    }
  };

  const handleSelectConv = async (convId: string) => {
    if (convId === activeConvId) return;
    await loadConversation(convId);
  };

  const getKindIcon = (kind: { type: string }) => {
    const k = KIND_ICONS[kind.type] || KIND_ICONS['general'];
    const Icon = k.icon;
    return <Icon size={14} style={{ color: k.color }} />;
  };

  // Conversations are loaded from the active workspace-scoped store after switchWorkspace.
  const visibleConvs = current
    ? conversations.slice(0, showAllConvs ? conversations.length : MAX_RECENT_CONVERSATIONS)
    : [];
  const hasMoreConvs = current && !showAllConvs && conversations.length > MAX_RECENT_CONVERSATIONS;

  return (
    <div className="flex h-full flex-col bg-[var(--bg-sidebar)]">
      {/* Brand Header */}
      <div className="flex items-center justify-between px-3 py-3">
        <div className="flex items-center gap-2">
          <BrandIcon size="md" />
          <span className="text-sm font-semibold tracking-tight text-[var(--text-primary)]">
            EchoCoWork
          </span>
          {current && (
            <div className="flex items-center gap-1">
              <button
                onClick={onNewTask}
                className="flex max-w-[118px] items-center gap-1 rounded-md bg-[var(--bg-hover)] px-1.5 py-0.5 text-[11px] font-medium text-[var(--text-secondary)] transition-colors hover:text-[var(--text-primary)]"
                title={`当前: ${current.name}\n${current.root}`}
              >
                <FolderOpen size={11} />
                {current.name}
              </button>
              <button
                onClick={(e) => handleOpenFolder(e, current)}
                className="rounded p-1 text-[var(--text-tertiary)] transition-colors hover:text-[var(--text-primary)]"
                title="在文件管理器中打开"
              >
                <FolderOpen size={13} />
              </button>
            </div>
          )}
        </div>
        <button
          onClick={toggleTheme}
          className="rounded-lg p-1.5 text-[var(--text-tertiary)] transition-colors hover:bg-[var(--bg-sidebar-hover)] hover:text-[var(--text-primary)]"
          title={theme === 'dark' ? '浅色模式' : '深色模式'}
        >
          {theme === 'dark' ? <Sun size={14} /> : <Moon size={14} />}
        </button>
      </div>

      {/* New Task + Search */}
      <div className="space-y-2 px-3 pb-3">
        <button
          onClick={onNewTask}
          className="flex w-full items-center gap-2 rounded-lg bg-[var(--action-soft)] px-3 py-2 text-sm font-medium text-[var(--text-on-soft-action)] transition-colors hover:bg-[var(--action-soft-hover)]"
        >
          <Plus size={15} />
          新建任务
        </button>

        {workspaces.length > 0 && (
          <div className="flex items-center gap-2 rounded-lg bg-[var(--bg-hover)] px-3 py-1.5 transition-colors focus-within:ring-1 focus-within:ring-[var(--border-focus)]">
            <Search size={13} className="shrink-0 text-[var(--text-tertiary)]" />
            <input
              ref={searchInputRef}
              value={searchQuery}
              onChange={(e) => setSearchQuery(e.target.value)}
              placeholder="搜索任务..."
              className="flex-1 bg-transparent text-xs text-[var(--text-primary)] outline-none placeholder:text-[var(--text-tertiary)]"
            />
            {searchQuery && (
              <button
                onClick={() => {
                  setSearchQuery('');
                  searchInputRef.current?.focus();
                }}
                className="shrink-0 rounded p-0.5 text-[var(--text-tertiary)] hover:text-[var(--text-primary)]"
              >
                <X size={12} />
              </button>
            )}
          </div>
        )}
      </div>

      {/* Search Results — conversation content matches */}
      {isSearchingContent && (
        <div className="border-b border-[var(--border-primary)] px-2 pb-2">
          <div className="px-1 py-1 text-[10px] font-semibold uppercase tracking-wider text-[var(--text-tertiary)]">
            搜索结果 ({searchResults.length})
          </div>
          {isSearching && (
            <div className="flex items-center gap-2 px-2 py-3">
              <Loader2 size={12} className="animate-spin text-[var(--text-tertiary)]" />
              <span className="text-[11px] text-[var(--text-tertiary)]">搜索中...</span>
            </div>
          )}
          {!isSearching && searchResults.length === 0 && (
            <div className="px-2 py-3 text-center">
              <Search size={16} className="mx-auto mb-1 text-[var(--text-tertiary)]" />
              <p className="text-[11px] text-[var(--text-tertiary)]">未找到匹配的对话</p>
            </div>
          )}
          {!isSearching &&
            searchResults.map((conv) => (
              <div
                key={conv.conversation_id}
                className="cursor-pointer rounded-md px-2 py-2 text-[12px] transition-colors hover:bg-[var(--bg-hover)]"
                onClick={() => handleSelectConv(conv.conversation_id)}
              >
                <div className="flex items-center gap-1.5">
                  <MessageSquare size={11} className="shrink-0 text-[var(--accent)]" />
                  <span className="truncate font-medium text-[var(--text-primary)]">
                    {conv.title || '新对话'}
                  </span>
                </div>
                <div className="mt-0.5 flex items-center gap-2 text-[10px] text-[var(--text-tertiary)] pl-[17px]">
                  <span>{conv.message_count} 条消息</span>
                  <span>{new Date(conv.updated_at).toLocaleDateString()}</span>
                </div>
              </div>
            ))}
        </div>
      )}

      {/* Task (Workspace) List — expandable with conversations */}
      <div className="flex-1 overflow-y-auto px-2 pb-2">
        {filtered.length === 0 && !searchQuery && (
          <div className="px-3 py-8 text-center">
            <FolderOpen size={24} className="mx-auto mb-2 text-[var(--text-tertiary)]" />
            <p className="text-xs text-[var(--text-tertiary)]">暂无任务，点击上方创建</p>
          </div>
        )}

        {filtered.length === 0 && searchQuery && (
          <div className="px-3 py-8 text-center">
            <Search size={24} className="mx-auto mb-2 text-[var(--text-tertiary)]" />
            <p className="text-xs text-[var(--text-tertiary)]">
              没有找到 &quot;{searchQuery}&quot;
            </p>
          </div>
        )}

        {filtered.map((ws) => {
          const isActive = current?.id === ws.id;
          const isExpanded = expandedId === ws.id && isActive;
          return (
            <div key={ws.id} className="mb-0.5">
              {/* Workspace row */}
              <div
                className={`group relative cursor-pointer rounded-lg transition-all
                  ${
                    isActive
                      ? 'bg-[var(--bg-sidebar-active)]'
                      : 'hover:bg-[var(--bg-sidebar-hover)]'
                  }`}
                onClick={() => handleSwitch(ws)}
              >
                <div className="flex items-center gap-2 px-2 py-2.5">
                  {/* Expand arrow */}
                  {isActive ? (
                    isExpanded ? (
                      <ChevronDown size={13} className="shrink-0 text-[var(--text-tertiary)]" />
                    ) : (
                      <ChevronRight size={13} className="shrink-0 text-[var(--text-tertiary)]" />
                    )
                  ) : (
                    getKindIcon(ws.kind)
                  )}

                  <div className="min-w-0 flex-1">
                    <div
                      className={`truncate text-[13px] ${isActive ? 'font-medium' : ''} text-[var(--text-primary)]`}
                    >
                      {ws.name}
                    </div>
                    <div className="truncate text-[11px] text-[var(--text-tertiary)]">
                      {ws.root}
                    </div>
                  </div>

                  {isActive && (
                    <div className="h-1.5 w-1.5 shrink-0 rounded-full bg-[var(--accent)]" />
                  )}

                  <div className="flex shrink-0 items-center gap-0.5 opacity-0 group-hover:opacity-100 transition-opacity">
                    <button
                      onClick={(e) => handleOpenFolder(e, ws)}
                      className="rounded p-1 text-[var(--text-tertiary)] transition-colors hover:text-[var(--text-primary)]"
                      title="打开文件夹"
                    >
                      <FolderOpen size={12} />
                    </button>
                    <button
                      onClick={(e) => handleDelete(ws.id, e)}
                      className="rounded p-1 text-[var(--text-tertiary)] transition-colors hover:text-[var(--color-error)]"
                      title="删除"
                    >
                      <Trash2 size={12} />
                    </button>
                  </div>
                </div>
              </div>

              {/* Expanded conversations */}
              {isExpanded && (
                <div className="ml-7 border-l border-[var(--border-primary)] pl-2 pb-1">
                  {isConvLoading && visibleConvs.length === 0 && (
                    <div className="flex items-center gap-2 py-2 px-1">
                      <Loader2 size={12} className="animate-spin text-[var(--text-tertiary)]" />
                      <span className="text-[11px] text-[var(--text-tertiary)]">加载中...</span>
                    </div>
                  )}

                  {!isConvLoading && visibleConvs.length === 0 && (
                    <div className="py-2 px-1 text-[11px] text-[var(--text-tertiary)]">
                      暂无对话，开始聊天吧
                    </div>
                  )}

                  {visibleConvs.map((conv) => (
                    <div
                      key={conv.id}
                      className={`cursor-pointer rounded-md px-2 py-1.5 text-[12px] transition-colors
                        ${
                          activeConvId === conv.id
                            ? 'bg-[var(--bg-sidebar-active)] text-[var(--text-primary)] font-medium'
                            : 'text-[var(--text-secondary)] hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]'
                        }`}
                      onClick={(e) => {
                        e.stopPropagation();
                        handleSelectConv(conv.id);
                      }}
                    >
                      <div className="flex items-center gap-1.5">
                        <MessageSquare size={11} className="shrink-0 text-[var(--text-tertiary)]" />
                        <span className="truncate">{conv.title || '新对话'}</span>
                      </div>
                      {conv.messageCount > 0 && (
                        <div className="mt-0.5 text-[10px] text-[var(--text-tertiary)] pl-[17px]">
                          {conv.messageCount} 条消息
                        </div>
                      )}
                    </div>
                  ))}

                  {hasMoreConvs && (
                    <div className="mt-1 text-center">
                      <button
                        type="button"
                        onClick={(e) => {
                          e.stopPropagation();
                          setShowAllConvs(true);
                        }}
                        className="text-[11px] text-[var(--accent)] cursor-pointer hover:underline"
                      >
                        查看更多 ({conversations.length - MAX_RECENT_CONVERSATIONS})...
                      </button>
                    </div>
                  )}
                </div>
              )}
            </div>
          );
        })}
      </div>

      {/* Bottom actions */}
      <div className="border-t border-[var(--border-primary)] px-2 py-2">
        <button
          onClick={toggleTerminal}
          className="flex w-full items-center gap-2.5 rounded-lg px-3 py-2 text-xs font-medium text-[var(--text-secondary)] transition-all hover:bg-[var(--bg-sidebar-hover)] hover:text-[var(--text-primary)]"
        >
          <Terminal size={14} />
          终端
        </button>
        <button
          onClick={openSettings}
          className="flex w-full items-center gap-2.5 rounded-lg px-3 py-2 text-xs font-medium text-[var(--text-secondary)] transition-all hover:bg-[var(--bg-sidebar-hover)] hover:text-[var(--text-primary)]"
        >
          <Settings size={14} />
          设置
        </button>
      </div>
    </div>
  );
}
