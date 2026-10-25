import { useState, useRef, useEffect, useMemo } from 'react';
import {
  Plus,
  Sun,
  Moon,
  Trash2,
  FolderOpen,
  Terminal,
  Settings,
  MessageSquare,
  Search,
  X,
  ChevronDown,
  ChevronRight,
  GitFork,
  Pencil,
  Loader2,
  RefreshCw,
  Archive,
  Server,
  CalendarClock,
  MessagesSquare,
} from 'lucide-react';
import { BrandIcon } from '../common/BrandIcon';
import { useUiStore } from '../../stores/uiStore';
import { useWorkspaceStore } from '../../stores/workspaceStore';
import { useConversationStore } from '../../stores/conversationStore';
import { conversationApi } from '../../api/endpoints';
import type { Workspace } from '../../api/endpoints';
import type { ConversationListItem } from '../../types/api';
import { fileSystem } from '../../lib/tauri-bridge';
import { workspaceIdForView, GLOBAL_WORKSPACE_ID } from '../../lib/viewAddress';
import { workspaceLocationLabel, workspaceIsRemote } from '../../lib/workspacePresentation';
import { useToastStore } from '../../stores/toastStore';
import { useWorkspaceViewStore } from '../../stores/workspaceViewStore';

const MAX_RECENT_CONVERSATIONS = 5;
const WORKSPACE_BATCH = 6;
const RECENT_PER_WORKSPACE = 8;
interface RecentTask {
  workspaceId: string;
  workspaceGeneration: string;
  workspaceName: string;
  conversation: ConversationListItem;
}
const SIDE_STATUS_LABEL = {
  idle: '空闲',
  queued: '排队',
  running: '运行中',
  completed: '已完成',
  failed: '失败',
  cancelled: '已取消',
  degraded: '需清理',
} as const;

export function LeftSidebar({ onNewTask }: { onNewTask: () => void }) {
  const theme = useUiStore((s) => s.theme);
  const toggleTheme = useUiStore((s) => s.toggleTheme);
  const openSettings = useUiStore((s) => s.openSettings);
  const setActiveSettingsTab = useUiStore((s) => s.setActiveSettingsTab);
  const toggleTerminal = useUiStore((s) => s.toggleTerminal);
  const openWorkspaceDialog = useUiStore((state) => state.openWorkspaceDialog);

  const workspaces = useWorkspaceStore((s) => s.workspaces);
  const current = useWorkspaceStore((s) => s.current);
  const isWorkspaceLoading = useWorkspaceStore((state) => state.isLoading);
  const switchTo = useWorkspaceStore((s) => s.switchTo);
  const deleteWorkspace = useWorkspaceStore((s) => s.delete);

  const conversations = useConversationStore((s) => s.conversations);
  const sideConversations = useConversationStore((s) => s.sideConversations);
  const activeConvId = useConversationStore((s) => s.activeId);
  const isConvLoading = useConversationStore((s) => s.isLoading);
  const loadConversation = useConversationStore((s) => s.loadConversation);
  const startNewConversation = useConversationStore((s) => s.startNew);
  const archiveConversation = useConversationStore((s) => s.archiveConversation);
  const deleteConversation = useConversationStore((s) => s.deleteConversation);
  const renameConversation = useConversationStore((s) => s.renameConversation);
  const retrySideConversation = useConversationStore((s) => s.retrySideConversation);

  const [expandedId, setExpandedId] = useState<string | null>(null);
  const [searchQuery, setSearchQuery] = useState('');
  const [searchResults, setSearchResults] = useState<ConversationListItem[]>([]);
  const [searchResultScope, setSearchResultScope] = useState('');
  const currentSearchScope = JSON.stringify([
    current?.id ?? GLOBAL_WORKSPACE_ID,
    current?.product_data_generation ?? GLOBAL_WORKSPACE_ID,
  ]);
  const [isSearching, setIsSearching] = useState(false);
  const [showAllConvs, setShowAllConvs] = useState(false);
  const [renamingSideId, setRenamingSideId] = useState<string | null>(null);
  const [sideTitleDraft, setSideTitleDraft] = useState('');
  const [isRenamingSide, setIsRenamingSide] = useState(false);
  const searchInputRef = useRef<HTMLInputElement>(null);
  const [recentTasks, setRecentTasks] = useState<RecentTask[]>([]);
  const [recentWorkspaceCount, setRecentWorkspaceCount] = useState(WORKSPACE_BATCH);
  const [recentVisibleCount, setRecentVisibleCount] = useState(4);
  const [recentLoading, setRecentLoading] = useState(false);
  const [recentError, setRecentError] = useState('');
  const recentScopes = useMemo(
    () => [
      {
        id: GLOBAL_WORKSPACE_ID,
        generation: GLOBAL_WORKSPACE_ID,
        name: '未分组任务',
        lastActive: '',
      },
      ...workspaces
        .map((workspace) => ({
          id: workspace.id,
          generation: workspace.product_data_generation,
          name: workspace.name,
          lastActive: workspace.last_active,
        }))
        .sort((a, b) =>
          a.id === current?.id
            ? -1
            : b.id === current?.id
              ? 1
              : b.lastActive.localeCompare(a.lastActive)
        ),
    ],
    [workspaces, current?.id]
  );
  useEffect(() => {
    let cancelled = false;
    const scopes = recentScopes.slice(0, recentWorkspaceCount);
    const rows: RecentTask[] = [];
    let next = 0;
    let failed = 0;
    setRecentLoading(true);
    setRecentError('');
    // Read only the explicitly requested finite batch, with two metadata reads at a time.
    const readNext = async () => {
      while (!cancelled && next < scopes.length) {
        const scope = scopes[next++];
        if (!scope) return;
        try {
          const items = await conversationApi.list(scope.id);
          if (cancelled) return;
          const actual = useWorkspaceStore
            .getState()
            .workspaces.find((item) => item.id === scope.id);
          if (
            scope.id !== GLOBAL_WORKSPACE_ID &&
            actual?.product_data_generation !== scope.generation
          )
            continue;
          rows.push(
            ...items
              .filter((item) => !item.archived && !item.side_conversation)
              .sort((a, b) => b.updated_at.localeCompare(a.updated_at))
              .slice(0, RECENT_PER_WORKSPACE)
              .map((conversation) => ({
                workspaceId: scope.id,
                workspaceGeneration: scope.generation,
                workspaceName: scope.name,
                conversation,
              }))
          );
        } catch {
          failed += 1;
        }
      }
    };
    void Promise.all([readNext(), readNext()]).then(() => {
      if (cancelled) return;
      setRecentTasks(
        rows.sort((a, b) => b.conversation.updated_at.localeCompare(a.conversation.updated_at))
      );
      setRecentError(failed ? `${failed} 个工作空间的最近任务暂时无法读取` : '');
      setRecentLoading(false);
    });
    return () => {
      cancelled = true;
    };
  }, [recentScopes, recentWorkspaceCount, conversations]);

  const openRecentTask = async (task: RecentTask) => {
    const epoch = useUiStore.getState().beginNavigation();
    const before = useWorkspaceStore.getState();
    const target = before.workspaces.find((workspace) => workspace.id === task.workspaceId);
    if (
      task.workspaceId !== GLOBAL_WORKSPACE_ID &&
      target?.product_data_generation !== task.workspaceGeneration
    ) {
      useToastStore.getState().addToast('error', '工作空间已变化，请重新选择任务');
      return;
    }
    try {
      const currentId = before.current?.id ?? GLOBAL_WORKSPACE_ID;
      const transition =
        currentId === task.workspaceId
          ? null
          : task.workspaceId === GLOBAL_WORKSPACE_ID
            ? before.exit()
            : before.switchTo(task.workspaceId);
      // switchTo detaches synchronously; later new-task actions must supersede this intent.
      const draftEpoch = useConversationStore.getState().newConversationEpoch;
      if (transition) await transition;
      const actual = useWorkspaceStore.getState().current;
      if (
        useUiStore.getState().navigationEpoch !== epoch ||
        useConversationStore.getState().newConversationEpoch !== draftEpoch
      )
        return;
      if (
        (actual?.id ?? GLOBAL_WORKSPACE_ID) !== task.workspaceId ||
        (actual?.product_data_generation ?? GLOBAL_WORKSPACE_ID) !== task.workspaceGeneration ||
        useConversationStore.getState().workspaceId !== task.workspaceId
      )
        return;
      await loadConversation(task.conversation.conversation_id);
      if (useUiStore.getState().navigationEpoch === epoch)
        useWorkspaceViewStore.getState().openChat();
    } catch (error) {
      if (useUiStore.getState().navigationEpoch === epoch)
        useToastStore
          .getState()
          .addToast('error', error instanceof Error ? error.message : '打开任务失败');
    }
  };

  // Debounced search across all conversation content
  useEffect(() => {
    if (!searchQuery.trim()) {
      setSearchResults([]);
      return;
    }
    const requestScope = JSON.stringify([
      current?.id ?? GLOBAL_WORKSPACE_ID,
      current?.product_data_generation ?? GLOBAL_WORKSPACE_ID,
    ]);
    const controller = new AbortController();
    const timer = setTimeout(async () => {
      if (controller.signal.aborted) return;
      setIsSearching(true);
      try {
        const results = await conversationApi.search(
          workspaceIdForView(current?.id),
          searchQuery.trim()
        );
        if (!controller.signal.aborted) {
          setSearchResultScope(requestScope);
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
  }, [current?.id, current?.product_data_generation, searchQuery]);

  useEffect(() => {
    setShowAllConvs(false);
    setRenamingSideId(null);
    setSideTitleDraft('');
  }, [current?.id]);

  useEffect(() => {
    if (
      isConvLoading ||
      !sideConversations.some(
        (conversation) => conversation.status === 'queued' || conversation.status === 'running'
      )
    ) {
      return;
    }
    const interval = window.setInterval(() => {
      void useConversationStore.getState().init(workspaceIdForView(current?.id));
    }, 1200);
    return () => window.clearInterval(interval);
  }, [current?.id, isConvLoading, sideConversations]);

  // Always filter workspace names; search results come from API when searching
  const filtered = searchQuery.trim()
    ? workspaces.filter((w) => w.name.toLowerCase().includes(searchQuery.toLowerCase()))
    : workspaces;
  const isSearchingContent = searchQuery.trim().length > 0;
  const visibleSearchResults =
    searchResultScope === currentSearchScope
      ? searchResults.filter((conversation) => !conversation.archived)
      : [];

  const handleSwitch = async (ws: Workspace) => {
    if (current?.id === ws.id) {
      // Already active — toggle expand
      setExpandedId(expandedId === ws.id ? null : ws.id);
      return;
    }
    const epoch = useUiStore.getState().beginNavigation();
    try {
      await switchTo(ws.id);
      const actual = useWorkspaceStore.getState().current;
      if (
        useUiStore.getState().navigationEpoch !== epoch ||
        actual?.id !== ws.id ||
        actual.product_data_generation !== ws.product_data_generation
      )
        return;
      setExpandedId(ws.id);
      useWorkspaceViewStore.getState().openChat();
    } catch (error) {
      if (useUiStore.getState().navigationEpoch === epoch)
        useToastStore
          .getState()
          .addToast('error', error instanceof Error ? error.message : '切换工作空间失败');
    }
  };

  const handleDelete = async (id: string, e: React.MouseEvent) => {
    e.stopPropagation();
    if (!confirm('确定移除此工作空间及其本机会话数据？项目代码文件不会被删除。')) return;
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
      await fileSystem.openPath(workspaceLocationLabel(ws));
    } catch (err) {
      console.error('Open folder failed:', err);
    }
  };

  const handleNewConversation = async (e: React.MouseEvent, ws: Workspace) => {
    e.stopPropagation();
    try {
      const epoch = useUiStore.getState().beginNavigation();
      if (current?.id !== ws.id) await switchTo(ws.id);
      const actual = useWorkspaceStore.getState().current;
      if (
        useUiStore.getState().navigationEpoch !== epoch ||
        actual?.id !== ws.id ||
        actual.product_data_generation !== ws.product_data_generation
      )
        return;
      await startNewConversation();
      if (useUiStore.getState().navigationEpoch !== epoch) return;
      useWorkspaceViewStore.getState().openChat();
      setExpandedId(ws.id);
      setShowAllConvs(false);
    } catch (err) {
      console.error('Start new conversation failed:', err);
    }
  };

  const handleSelectConv = async (convId: string) => {
    if (convId === activeConvId) return;
    const epoch = useUiStore.getState().beginNavigation();
    await loadConversation(convId);
    if (useUiStore.getState().navigationEpoch === epoch)
      useWorkspaceViewStore.getState().openChat();
  };

  const handleSearchResult = async (id: string, scope: string) => {
    const live = useWorkspaceStore.getState().current;
    if (
      scope !==
      JSON.stringify([
        live?.id ?? GLOBAL_WORKSPACE_ID,
        live?.product_data_generation ?? GLOBAL_WORKSPACE_ID,
      ])
    )
      return;
    await handleSelectConv(id);
  };

  const handleArchiveConversation = async (id: string, e: React.MouseEvent) => {
    e.stopPropagation();
    try {
      await archiveConversation(id);
    } catch (error) {
      console.error('Archive conversation failed:', error);
    }
  };

  const handleDeleteSideConversation = async (
    id: string,
    title: string,
    event: React.MouseEvent
  ) => {
    event.stopPropagation();
    if (!confirm(`确定删除支线对话“${title}”？主对话不会被删除。`)) return;
    try {
      await deleteConversation(id);
      await useConversationStore.getState().init(workspaceIdForView(current?.id));
    } catch (error) {
      console.error('Delete Side Conversation failed:', error);
    }
  };

  const startRenamingSideConversation = (id: string, title: string, event: React.MouseEvent) => {
    event.stopPropagation();
    setRenamingSideId(id);
    setSideTitleDraft(title);
  };

  const commitSideConversationTitle = async (id: string) => {
    const title = sideTitleDraft.trim();
    if (!title || isRenamingSide) {
      if (!title) setRenamingSideId(null);
      return;
    }
    setIsRenamingSide(true);
    try {
      await renameConversation(id, title);
      setRenamingSideId(null);
      setSideTitleDraft('');
    } catch (error) {
      console.error('Rename Side Conversation failed:', error);
    } finally {
      setIsRenamingSide(false);
    }
  };

  const handleRetrySideConversation = async (id: string, event: React.MouseEvent) => {
    event.stopPropagation();
    try {
      await retrySideConversation(id);
    } catch (error) {
      console.error('Retry Side Conversation failed:', error);
    }
  };

  // Conversations are loaded from the active workspace-scoped store after switchWorkspace.
  const sideConversationIds = new Set(
    sideConversations.map((conversation) => conversation.conversation_id)
  );
  const filteredConversations = conversations.filter(
    (conversation) => !conversation.archived && !sideConversationIds.has(conversation.id)
  );
  const visibleConvs = current
    ? filteredConversations.slice(
        0,
        showAllConvs ? filteredConversations.length : MAX_RECENT_CONVERSATIONS
      )
    : [];
  const hasMoreConvs =
    current && !showAllConvs && filteredConversations.length > MAX_RECENT_CONVERSATIONS;

  return (
    <div className="flex h-full flex-col bg-[var(--bg-sidebar)] text-[var(--text-secondary)]">
      {/* Brand Header */}
      <div className="flex h-14 items-center justify-between px-4">
        <div className="flex items-center gap-2">
          <BrandIcon size="md" />
          <span className="text-sm font-semibold tracking-tight text-[var(--text-primary)]">
            EKO
          </span>
        </div>
        <button
          onClick={toggleTheme}
          className="rounded-md p-1.5 text-[var(--text-tertiary)] transition-colors hover:bg-[var(--bg-sidebar-hover)] hover:text-[var(--text-primary)]"
          title={theme === 'dark' ? '浅色模式' : '深色模式'}
        >
          {theme === 'dark' ? <Sun size={14} /> : <Moon size={14} />}
        </button>
      </div>

      {/* New Task + Search */}
      <div className="space-y-2 px-3 pb-4">
        <button
          type="button"
          onClick={() => onNewTask()}
          disabled={isWorkspaceLoading}
          title={isWorkspaceLoading ? '工作空间正在切换，请稍候' : '在当前工作空间新建任务'}
          className="disabled:cursor-wait disabled:opacity-60 flex h-10 w-full items-center justify-start gap-2.5 rounded-xl bg-[var(--bg-sidebar-active)] px-3 text-sm font-medium text-[var(--text-primary)] transition-colors hover:bg-[var(--bg-sidebar-hover)]"
        >
          <Plus size={16} />
          新建任务
        </button>

        {workspaces.length > 0 && (
          <div className="flex h-9 items-center gap-2 rounded-lg border border-transparent bg-[var(--bg-hover)] px-3 transition-colors focus-within:border-[var(--border-focus)]">
            <Search size={13} className="shrink-0 text-[var(--text-tertiary)]" />
            <input
              ref={searchInputRef}
              value={searchQuery}
              onChange={(e) => setSearchQuery(e.target.value)}
              placeholder="搜索任务或工作空间"
              aria-label="搜索任务或工作空间"
              className="flex-1 bg-transparent text-xs text-[var(--text-primary)] outline-none placeholder:text-[var(--text-tertiary)]"
            />
            {searchQuery && (
              <button
                type="button"
                aria-label="清除搜索"
                onClick={() => {
                  setSearchQuery('');
                  searchInputRef.current?.focus();
                }}
                className="shrink-0 rounded-md p-0.5 text-[var(--text-tertiary)] hover:text-[var(--text-primary)]"
              >
                <X size={12} />
              </button>
            )}
          </div>
        )}
      </div>

      <nav aria-label="后台服务" className="space-y-0.5 px-3 pb-3">
        <button
          type="button"
          onClick={() => setActiveSettingsTab('channels')}
          className="flex h-8 w-full items-center gap-2.5 rounded-lg px-3 text-[13px] hover:bg-[var(--bg-sidebar-hover)]"
        >
          <MessagesSquare size={15} />
          IM 渠道
        </button>
        <button
          type="button"
          onClick={() => setActiveSettingsTab('scheduler')}
          className="flex h-8 w-full items-center gap-2.5 rounded-lg px-3 text-[13px] hover:bg-[var(--bg-sidebar-hover)]"
        >
          <CalendarClock size={15} />
          定时任务
        </button>
      </nav>

      {/* Search Results — conversation content matches */}
      {isSearchingContent && (
        <div className="border-b border-[var(--border-primary)] px-2 pb-2">
          <div className="px-1 py-1 text-xs font-semibold uppercase text-[var(--text-tertiary)]">
            搜索结果 ({visibleSearchResults.length})
          </div>
          {isSearching && (
            <div className="flex items-center gap-2 px-2 py-3">
              <Loader2 size={12} className="animate-spin text-[var(--text-tertiary)]" />
              <span className="text-xs text-[var(--text-tertiary)]">搜索中...</span>
            </div>
          )}
          {!isSearching && visibleSearchResults.length === 0 && (
            <div className="px-2 py-3 text-center">
              <Search size={16} className="mx-auto mb-1 text-[var(--text-tertiary)]" />
              <p className="text-xs text-[var(--text-tertiary)]">未找到匹配的对话</p>
            </div>
          )}
          {!isSearching &&
            visibleSearchResults.map((conv) => (
              <div
                key={conv.conversation_id}
                role="button"
                tabIndex={0}
                className="cursor-pointer rounded-md px-2 py-2 text-[13px] transition-colors hover:bg-[var(--bg-hover)]"
                onClick={() => void handleSearchResult(conv.conversation_id, searchResultScope)}
                onKeyDown={(event) => {
                  if (event.key === 'Enter' || event.key === ' ') {
                    event.preventDefault();
                    void handleSearchResult(conv.conversation_id, searchResultScope);
                  }
                }}
              >
                <div className="flex items-center gap-1.5">
                  <MessageSquare size={11} className="shrink-0 text-[var(--accent)]" />
                  <span className="truncate font-medium text-[var(--text-primary)]">
                    {conv.title || '新对话'}
                  </span>
                </div>
                <div className="mt-0.5 flex items-center gap-2 text-xs text-[var(--text-tertiary)] pl-[17px]">
                  <span>{conv.message_count} 条消息</span>
                  <span>{new Date(conv.updated_at).toLocaleDateString()}</span>
                </div>
              </div>
            ))}
        </div>
      )}

      {/* Task (Workspace) List — expandable with conversations */}
      <div className="flex-1 overflow-y-auto px-2 pb-2">
        {!isSearchingContent && (
          <section aria-label="最近任务" className="mb-3">
            <div className="flex items-center justify-between px-3 pb-2 text-[11px] font-medium text-[var(--text-tertiary)]">
              <h2>最近任务</h2>
              {recentLoading && (
                <Loader2 size={12} className="animate-spin" aria-label="读取最近任务" />
              )}
            </div>
            {recentTasks.slice(0, recentVisibleCount).map((task) => (
              <button
                type="button"
                key={`${task.workspaceId}:${task.workspaceGeneration}:${task.conversation.conversation_id}`}
                onClick={() => void openRecentTask(task)}
                aria-label={`打开任务 ${task.conversation.title || '新任务'}，${task.workspaceName}`}
                title={`${task.conversation.title || '新任务'} · ${task.workspaceName}`}
                className={`group flex min-h-8 w-full items-center gap-2.5 rounded-lg px-3 py-1.5 text-left text-[13px] transition-colors hover:bg-[var(--bg-sidebar-hover)] ${activeConvId === task.conversation.conversation_id && (current?.id ?? GLOBAL_WORKSPACE_ID) === task.workspaceId ? 'bg-[var(--bg-sidebar-active)] text-[var(--text-primary)]' : ''}`}
              >
                <MessageSquare size={13} className="shrink-0 text-[var(--text-tertiary)]" />
                <span className="min-w-0 flex-1 truncate">
                  {task.conversation.title || '新任务'}
                </span>
                <span className="max-w-[56px] shrink-0 truncate text-[10px] text-[var(--text-tertiary)]">
                  {task.workspaceName}
                </span>
              </button>
            ))}
            {!recentLoading && recentTasks.length === 0 && (
              <p className="px-3 py-2 text-xs text-[var(--text-tertiary)]">从一个新任务开始</p>
            )}
            {recentError && (
              <p role="status" className="px-3 py-2 text-xs text-[var(--color-warning-text)]">
                {recentError}
              </p>
            )}
            {(recentTasks.length > recentVisibleCount ||
              recentWorkspaceCount < recentScopes.length) && (
              <button
                type="button"
                disabled={recentLoading}
                onClick={() => {
                  setRecentVisibleCount((count) => count + 8);
                  setRecentWorkspaceCount((count) => count + WORKSPACE_BATCH);
                }}
                className="px-3 py-2 text-xs text-[var(--text-tertiary)] hover:text-[var(--text-primary)]"
              >
                查看更多任务
              </button>
            )}
          </section>
        )}
        <div className="flex items-center justify-between px-3 pb-2 text-[11px] font-medium text-[var(--text-tertiary)]">
          <h2>工作空间</h2>
          <button
            type="button"
            onClick={() => openWorkspaceDialog()}
            aria-label="添加工作空间"
            title="添加工作空间"
            className="rounded-md p-1 hover:bg-[var(--bg-sidebar-hover)]"
          >
            <Plus size={13} />
          </button>
        </div>
        {filtered.length === 0 && !searchQuery && (
          <div className="px-3 py-8 text-center">
            <FolderOpen size={24} className="mx-auto mb-2 text-[var(--text-tertiary)]" />
            <p className="text-xs text-[var(--text-tertiary)]">还没有工作空间</p>
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
                role="button"
                tabIndex={0}
                aria-label={`打开工作空间 ${ws.name}`}
                className={`group relative cursor-pointer rounded-md transition-colors
                  ${
                    isActive
                      ? 'bg-[var(--bg-sidebar-active)] text-[var(--text-primary)]'
                      : 'hover:bg-[var(--bg-sidebar-hover)]'
                  }`}
                onClick={() => handleSwitch(ws)}
                onKeyDown={(event) => {
                  if (event.target !== event.currentTarget) return;
                  if (event.key === 'Enter' || event.key === ' ') {
                    event.preventDefault();
                    handleSwitch(ws);
                  }
                }}
              >
                <div className="flex h-9 items-center gap-2 px-3">
                  {/* Expand arrow */}
                  {isActive ? (
                    isExpanded ? (
                      <ChevronDown size={13} className="shrink-0 text-[var(--text-tertiary)]" />
                    ) : (
                      <ChevronRight size={13} className="shrink-0 text-[var(--text-tertiary)]" />
                    )
                  ) : workspaceIsRemote(ws) ? (
                    <Server size={14} className="shrink-0" />
                  ) : (
                    <FolderOpen size={14} className="shrink-0" />
                  )}

                  <div
                    className={`min-w-0 flex-1 truncate text-sm ${isActive ? 'font-medium' : ''}`}
                    title={workspaceLocationLabel(ws)}
                  >
                    {ws.name}
                  </div>

                  <div className="flex shrink-0 items-center gap-0.5 opacity-0 group-hover:opacity-100 group-focus-within:opacity-100 transition-opacity">
                    <button
                      onClick={(e) => handleNewConversation(e, ws)}
                      className="rounded-md p-1 text-[var(--text-tertiary)] transition-colors hover:text-[var(--accent)]"
                      title="新建会话"
                    >
                      <Plus size={12} />
                    </button>
                    {!workspaceIsRemote(ws) && (
                      <button
                        onClick={(e) => handleOpenFolder(e, ws)}
                        className="rounded-md p-1 text-[var(--text-tertiary)] transition-colors hover:text-[var(--text-primary)]"
                        title="打开文件夹"
                      >
                        <FolderOpen size={12} />
                      </button>
                    )}
                    <button
                      onClick={(e) => handleDelete(ws.id, e)}
                      className="rounded-md p-1 text-[var(--text-tertiary)] transition-colors hover:text-[var(--color-error)]"
                      title="删除"
                    >
                      <Trash2 size={12} />
                    </button>
                  </div>
                </div>
              </div>

              {/* Expanded conversations */}
              {isExpanded && (
                <div className="ml-7 border-l border-[var(--border-primary)] pb-1 pl-2">
                  <div className="mb-1 flex items-center justify-between px-2 py-1">
                    <span className="text-xs font-medium text-[var(--text-tertiary)]">
                      最近会话
                    </span>
                    <button
                      type="button"
                      onClick={(e) => {
                        e.stopPropagation();
                        openSettings();
                        setActiveSettingsTab('archives');
                      }}
                      className="flex items-center gap-1 rounded-md px-1.5 py-1 text-xs text-[var(--text-tertiary)] hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]"
                      title="管理已归档会话"
                    >
                      <Archive size={11} />
                      管理归档
                    </button>
                  </div>
                  {isConvLoading && visibleConvs.length === 0 && (
                    <div className="flex items-center gap-2 py-2 px-1">
                      <Loader2 size={12} className="animate-spin text-[var(--text-tertiary)]" />
                      <span className="text-xs text-[var(--text-tertiary)]">加载中...</span>
                    </div>
                  )}

                  {!isConvLoading && visibleConvs.length === 0 && (
                    <div className="py-2 px-1 text-xs text-[var(--text-tertiary)]">
                      暂无对话，开始聊天吧
                    </div>
                  )}

                  {visibleConvs.map((conv) => {
                    const children = sideConversations.filter(
                      (entry) => entry.parent_conversation_id === conv.id
                    );
                    return (
                      <div key={conv.id}>
                        <div
                          className={`group relative rounded-md text-[13px] transition-colors
                            ${
                              activeConvId === conv.id
                                ? 'bg-[var(--bg-sidebar-active)] font-medium text-[var(--text-primary)]'
                                : 'text-[var(--text-secondary)] hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]'
                            }`}
                        >
                          <button
                            type="button"
                            aria-label={`打开会话 ${conv.title || '新对话'}`}
                            className="block w-full cursor-pointer px-2 py-1.5 pr-10 text-left"
                            onClick={(e) => {
                              e.stopPropagation();
                              void handleSelectConv(conv.id);
                            }}
                          >
                            <div className="flex min-w-0 items-center gap-1.5">
                              <MessageSquare
                                size={11}
                                className="shrink-0 text-[var(--text-tertiary)]"
                              />
                              <span className="min-w-0 flex-1 truncate">
                                {conv.title || '新对话'}
                              </span>
                            </div>
                            {conv.messageCount > 0 && (
                              <div className="mt-0.5 pl-[17px] text-xs text-[var(--text-tertiary)]">
                                {conv.messageCount} 条消息
                              </div>
                            )}
                          </button>
                          <div className="absolute right-1 top-1/2 flex -translate-y-1/2 items-center gap-0.5 opacity-0 transition-opacity group-hover:opacity-100 group-focus-within:opacity-100 focus-within:opacity-100">
                            <button
                              type="button"
                              onClick={(e) => handleArchiveConversation(conv.id, e)}
                              className="rounded p-1 text-[var(--text-tertiary)] hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]"
                              title="归档会话"
                              aria-label={`归档会话 ${conv.title || '新对话'}`}
                            >
                              <Archive size={11} />
                            </button>
                          </div>
                        </div>
                        {children.map((child) => {
                          const childMeta = conversations.find(
                            (item) => item.id === child.conversation_id
                          );
                          return (
                            <div
                              key={child.conversation_id}
                              className={`group relative ml-3 rounded-md border-l border-[var(--border-secondary)] pl-1 text-xs transition-colors ${
                                activeConvId === child.conversation_id
                                  ? 'bg-[var(--bg-sidebar-active)] font-medium text-[var(--text-primary)]'
                                  : 'text-[var(--text-tertiary)] hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]'
                              }`}
                            >
                              {renamingSideId === child.conversation_id ? (
                                <input
                                  autoFocus
                                  value={sideTitleDraft}
                                  maxLength={160}
                                  disabled={isRenamingSide}
                                  onClick={(event) => event.stopPropagation()}
                                  onChange={(event) => setSideTitleDraft(event.target.value)}
                                  onBlur={() =>
                                    void commitSideConversationTitle(child.conversation_id)
                                  }
                                  onKeyDown={(event) => {
                                    event.stopPropagation();
                                    if (event.key === 'Enter') {
                                      event.preventDefault();
                                      void commitSideConversationTitle(child.conversation_id);
                                    } else if (event.key === 'Escape') {
                                      setRenamingSideId(null);
                                      setSideTitleDraft('');
                                    }
                                  }}
                                  aria-label={`重命名支线对话 ${child.title}`}
                                  className="mx-2 my-1 h-6 w-[calc(100%_-_1rem)] rounded border border-[var(--accent)] bg-[var(--bg-primary)] px-1.5 text-xs text-[var(--text-primary)] outline-none disabled:opacity-60"
                                />
                              ) : (
                                <>
                                  <button
                                    type="button"
                                    onClick={(event) => {
                                      event.stopPropagation();
                                      if (!child.degraded)
                                        void handleSelectConv(child.conversation_id);
                                    }}
                                    disabled={child.degraded}
                                    className={`flex min-h-7 w-full items-center gap-1.5 px-2 text-left disabled:cursor-not-allowed ${child.launch_error ? 'pr-16' : 'pr-12'}`}
                                    aria-label={`打开支线对话 ${child.title}`}
                                  >
                                    <GitFork size={10} className="shrink-0 text-[var(--accent)]" />
                                    <span className="min-w-0 flex-1 truncate">{child.title}</span>
                                    <span
                                      className={`shrink-0 text-[11px] ${child.status === 'failed' || child.status === 'degraded' ? 'text-[var(--color-error)]' : 'text-[var(--text-tertiary)]'}`}
                                    >
                                      {SIDE_STATUS_LABEL[child.status]}
                                    </span>
                                    {child.unread_count > 0 &&
                                      activeConvId !== child.conversation_id && (
                                        <span
                                          className="min-w-3 shrink-0 rounded-full bg-[var(--accent)] px-1 text-center text-[8px] leading-3 text-white"
                                          aria-label={`${child.unread_count} 条未读更新`}
                                        >
                                          {child.unread_count > 9 ? '9+' : child.unread_count}
                                        </span>
                                      )}
                                    {!child.degraded && childMeta && childMeta.messageCount > 0 && (
                                      <span className="shrink-0 text-[11px] text-[var(--text-tertiary)]">
                                        {childMeta.messageCount}
                                      </span>
                                    )}
                                  </button>
                                  <div className="absolute right-1 top-1/2 flex -translate-y-1/2 items-center opacity-0 transition-opacity group-hover:opacity-100 group-focus-within:opacity-100 focus-within:opacity-100">
                                    {child.launch_error && (
                                      <button
                                        type="button"
                                        onClick={(event) =>
                                          void handleRetrySideConversation(
                                            child.conversation_id,
                                            event
                                          )
                                        }
                                        className="flex h-5 w-5 items-center justify-center rounded text-[var(--color-error)] hover:bg-[var(--bg-hover)]"
                                        title={`重试支线首轮：${child.launch_error}`}
                                        aria-label={`重试支线对话 ${child.title}`}
                                      >
                                        <RefreshCw size={10} />
                                      </button>
                                    )}
                                    {!child.degraded && (
                                      <button
                                        type="button"
                                        onClick={(event) =>
                                          startRenamingSideConversation(
                                            child.conversation_id,
                                            child.title,
                                            event
                                          )
                                        }
                                        className="flex h-5 w-5 items-center justify-center rounded text-[var(--text-tertiary)] hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]"
                                        title="重命名支线对话"
                                        aria-label={`重命名支线对话 ${child.title}`}
                                      >
                                        <Pencil size={10} />
                                      </button>
                                    )}
                                    <button
                                      type="button"
                                      onClick={(event) =>
                                        void handleDeleteSideConversation(
                                          child.conversation_id,
                                          child.title,
                                          event
                                        )
                                      }
                                      className="flex h-5 w-5 items-center justify-center rounded text-[var(--text-tertiary)] hover:bg-[var(--bg-hover)] hover:text-[var(--color-error)]"
                                      title="删除支线对话"
                                      aria-label={`删除支线对话 ${child.title}`}
                                    >
                                      <Trash2 size={10} />
                                    </button>
                                  </div>
                                </>
                              )}
                            </div>
                          );
                        })}
                      </div>
                    );
                  })}

                  {hasMoreConvs && (
                    <div className="mt-1 text-center">
                      <button
                        type="button"
                        onClick={(e) => {
                          e.stopPropagation();
                          setShowAllConvs(true);
                        }}
                        className="text-xs text-[var(--accent)] cursor-pointer hover:underline"
                      >
                        查看更多 ({filteredConversations.length - MAX_RECENT_CONVERSATIONS})...
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
      <div className="space-y-1 border-t border-[var(--border-primary)] px-3 py-3">
        <button
          onClick={toggleTerminal}
          className="flex w-full items-center gap-2.5 rounded-lg px-3 py-2 text-sm font-medium text-[var(--text-secondary)] transition-colors hover:bg-[var(--bg-sidebar-hover)] hover:text-[var(--text-primary)]"
        >
          <Terminal size={14} />
          终端
        </button>
        <button
          onClick={openSettings}
          className="flex w-full items-center gap-2.5 rounded-lg px-3 py-2 text-sm font-medium text-[var(--text-secondary)] transition-colors hover:bg-[var(--bg-sidebar-hover)] hover:text-[var(--text-primary)]"
        >
          <Settings size={14} />
          设置
        </button>
      </div>
    </div>
  );
}
