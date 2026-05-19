import { useEffect, useState, useRef } from 'react';
import { Plus, Sun, Moon, Trash2, MessageSquare, Pencil, Check, X, Search, Download, Settings } from 'lucide-react';
import { BrandIcon } from '../common/BrandIcon';
import { useChatStore } from '../../stores/chatStore';
import { useConversationStore, type ConversationGroup } from '../../stores/conversationStore';
import { useUiStore } from '../../stores/uiStore';

export function LeftSidebar() {
  const messages = useChatStore((s) => s.messages);
  const theme = useUiStore((s) => s.theme);
  const toggleTheme = useUiStore((s) => s.toggleTheme);
  const openSettings = useUiStore((s) => s.openSettings);

  const conversations = useConversationStore((s) => s.conversations);
  const activeId = useConversationStore((s) => s.activeId);
  const isLoading = useConversationStore((s) => s.isLoading);
  const init = useConversationStore((s) => s.init);
  const loadConversation = useConversationStore((s) => s.loadConversation);
  const deleteConversation = useConversationStore((s) => s.deleteConversation);
  const renameConversation = useConversationStore((s) => s.renameConversation);
  const startNew = useConversationStore((s) => s.startNew);
  const getGrouped = useConversationStore((s) => s.getGroupedConversations);

  const [hoveredId, setHoveredId] = useState<string | null>(null);
  const [editingId, setEditingId] = useState<string | null>(null);
  const [editTitle, setEditTitle] = useState('');
  const [searchQuery, setSearchQuery] = useState('');
  const editInputRef = useRef<HTMLInputElement>(null);
  const searchInputRef = useRef<HTMLInputElement>(null);

  useEffect(() => { init(); }, [init]);

  useEffect(() => {
    if (editingId && editInputRef.current) {
      editInputRef.current.focus();
      editInputRef.current.select();
    }
  }, [editingId]);

  const allGroups = getGrouped();
  const groups = searchQuery.trim()
    ? allGroups.map(g => ({
        ...g,
        conversations: g.conversations.filter(c =>
          c.title.toLowerCase().includes(searchQuery.toLowerCase())
        ),
      })).filter(g => g.conversations.length > 0)
    : allGroups;

  const handleSelect = async (id: string) => {
    if (id === activeId) return;
    await loadConversation(id);
  };

  const handleStartEdit = (id: string, e: React.MouseEvent) => {
    e.stopPropagation();
    const conv = conversations.find((c) => c.id === id);
    if (conv) {
      setEditingId(id);
      setEditTitle(conv.title);
    }
  };

  const handleSaveEdit = () => {
    if (editingId && editTitle.trim()) {
      renameConversation(editingId, editTitle.trim());
    }
    setEditingId(null);
  };

  const handleDelete = (id: string, e: React.MouseEvent) => {
    e.stopPropagation();
    deleteConversation(id);
  };

  const handleExport = async (id: string, title: string, e: React.MouseEvent) => {
    e.stopPropagation();
    try {
      const { conversationApi } = await import('../../api/endpoints');
      const res = await conversationApi.export(id);
      const blob = new Blob([res.content], { type: 'text/markdown' });
      const url = URL.createObjectURL(blob);
      const a = document.createElement('a');
      a.href = url;
      a.download = `${title.replace(/[^a-zA-Z0-9\u4e00-\u9fff]/g, '_')}.md`;
      a.click();
      URL.revokeObjectURL(url);
    } catch (err) {
      console.error('Export failed:', err);
    }
  };

  const isCurrentChatActive = !activeId && messages.length > 0;

  return (
    <div className="flex h-full flex-col bg-[var(--bg-sidebar)]">
      {/* Brand Header */}
      <div className="flex items-center justify-between border-b border-[var(--border-primary)] px-3 py-2.5">
        <div className="flex items-center gap-2">
          <BrandIcon size="md" />
          <span className="text-sm font-semibold tracking-tight text-[var(--text-primary)]">
            Echo Agent
          </span>
        </div>
        <button
          onClick={toggleTheme}
          className="rounded-lg p-1.5 text-[var(--text-tertiary)] transition-colors hover:bg-[var(--bg-sidebar-hover)] hover:text-[var(--text-primary)]"
          title={theme === 'dark' ? '浅色模式' : '深色模式'}
        >
          {theme === 'dark' ? <Sun size={14} /> : <Moon size={14} />}
        </button>
      </div>

      {/* New Chat + Search */}
      <div className="space-y-2 px-3 pt-3 pb-2">
        <button
          onClick={startNew}
          className="flex w-full items-center gap-2 rounded-lg border border-[var(--border-primary)] px-3 py-2 text-sm font-medium text-[var(--text-primary)] transition-all hover:bg-[var(--bg-sidebar-hover)]"
        >
          <Plus size={15} />
          新建对话
        </button>

        {conversations.length > 0 && (
          <div className="flex items-center gap-2 rounded-lg border border-[var(--border-primary)] bg-[var(--bg-hover)] px-3 py-1.5 transition-colors focus-within:border-[var(--border-focus)]">
            <Search size={13} className="shrink-0 text-[var(--text-tertiary)]" />
            <input
              ref={searchInputRef}
              value={searchQuery}
              onChange={(e) => setSearchQuery(e.target.value)}
              placeholder="搜索对话..."
              className="flex-1 bg-transparent text-xs text-[var(--text-primary)] outline-none placeholder:text-[var(--text-tertiary)]"
            />
            {searchQuery && (
              <button onClick={() => { setSearchQuery(''); searchInputRef.current?.focus(); }}
                className="shrink-0 rounded p-0.5 text-[var(--text-tertiary)] hover:text-[var(--text-primary)]"
              >
                <X size={12} />
              </button>
            )}
          </div>
        )}
      </div>

      {/* Current Chat */}
      <div className="px-3 pb-1">
        <button
          onClick={() => { if (!isCurrentChatActive) startNew(); }}
          className={`flex w-full items-center gap-2.5 rounded-lg px-3 py-2.5 text-left transition-colors
            ${isCurrentChatActive
              ? 'cursor-default bg-[var(--bg-sidebar-active)] pl-[9px] border-l-[3px] border-l-[var(--accent)]'
              : 'border-l-[3px] border-l-transparent hover:bg-[var(--bg-sidebar-hover)]'}`}
        >
          <MessageSquare size={15} className="shrink-0 text-[var(--accent)]" />
          <div className="min-w-0 flex-1">
            <div className="truncate text-[13px] font-medium text-[var(--text-primary)]">
              今天
            </div>
            <div className="text-xs text-[var(--text-tertiary)]">
              {messages.length > 0 ? `${messages.length} 条消息` : '空'}
            </div>
          </div>
          {isCurrentChatActive && (
            <div className="h-2 w-2 shrink-0 rounded-full bg-[var(--accent)]" />
          )}
        </button>
      </div>

      {/* Conversation Groups */}
      <div className="flex-1 overflow-y-auto px-2 pb-2">
        {groups.map((group) => (
          <ConversationGroupSection
            key={group.label}
            group={group}
            activeId={activeId}
            hoveredId={hoveredId}
            editingId={editingId}
            editTitle={editTitle}
            editInputRef={editInputRef}
            isLoading={isLoading}
            onSelect={handleSelect}
            onHover={setHoveredId}
            onStartEdit={handleStartEdit}
            onSaveEdit={handleSaveEdit}
            onCancelEdit={() => setEditingId(null)}
            onEditTitleChange={setEditTitle}
            onDelete={handleDelete}
            onExport={handleExport}
          />
        ))}

        {groups.length === 0 && searchQuery && (
          <div className="px-3 py-8 text-center">
            <Search size={24} className="mx-auto mb-2 text-[var(--text-tertiary)]" />
            <p className="text-xs text-[var(--text-tertiary)]">
              没有找到 &quot;{searchQuery}&quot;
            </p>
          </div>
        )}

        {groups.length === 0 && !searchQuery && conversations.length === 0 && (
          <div className="px-3 py-8 text-center">
            <MessageSquare size={24} className="mx-auto mb-2 text-[var(--text-tertiary)]" />
            <p className="text-xs text-[var(--text-tertiary)]">暂无对话</p>
          </div>
        )}
      </div>

      {/* Bottom actions */}
      <div className="border-t border-[var(--border-primary)] px-2 py-2">
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

function ConversationGroupSection({
  group,
  activeId,
  hoveredId,
  editingId,
  editTitle,
  editInputRef,
  isLoading,
  onSelect,
  onHover,
  onStartEdit,
  onSaveEdit,
  onCancelEdit,
  onEditTitleChange,
  onDelete,
  onExport,
}: {
  group: ConversationGroup;
  activeId: string | null;
  hoveredId: string | null;
  editingId: string | null;
  editTitle: string;
  editInputRef: React.RefObject<HTMLInputElement | null>;
  isLoading: boolean;
  onSelect: (id: string) => void;
  onHover: (id: string | null) => void;
  onStartEdit: (id: string, e: React.MouseEvent) => void;
  onSaveEdit: () => void;
  onCancelEdit: () => void;
  onEditTitleChange: (v: string) => void;
  onDelete: (id: string, e: React.MouseEvent) => void;
  onExport: (id: string, title: string, e: React.MouseEvent) => void;
}) {
  return (
    <div className="mt-1">
      <div className="px-3 py-1.5 text-[11px] font-semibold uppercase tracking-wider text-[var(--text-tertiary)]">
        {group.label}
      </div>
      {group.conversations.map((conv) => (
        <ConversationItem
          key={conv.id}
          id={conv.id}
          title={conv.title}
          isActive={activeId === conv.id}
          isHovered={hoveredId === conv.id}
          isEditing={editingId === conv.id}
          editTitle={editTitle}
          editInputRef={editInputRef}
          isLoading={isLoading && activeId === conv.id}
          onSelect={onSelect}
          onHover={onHover}
          onStartEdit={onStartEdit}
          onSaveEdit={onSaveEdit}
          onCancelEdit={onCancelEdit}
          onEditTitleChange={onEditTitleChange}
          onDelete={onDelete}
          onExport={onExport}
        />
      ))}
    </div>
  );
}

function ConversationItem({
  id,
  title,
  isActive,
  isHovered,
  isEditing,
  editTitle,
  editInputRef,
  isLoading: loading,
  onSelect,
  onHover,
  onStartEdit,
  onSaveEdit,
  onCancelEdit,
  onEditTitleChange,
  onDelete,
  onExport,
}: {
  id: string;
  title: string;
  isActive: boolean;
  isHovered: boolean;
  isEditing: boolean;
  editTitle: string;
  editInputRef: React.RefObject<HTMLInputElement | null>;
  isLoading: boolean;
  onSelect: (id: string) => void;
  onHover: (id: string | null) => void;
  onStartEdit: (id: string, e: React.MouseEvent) => void;
  onSaveEdit: () => void;
  onCancelEdit: () => void;
  onEditTitleChange: (v: string) => void;
  onDelete: (id: string, e: React.MouseEvent) => void;
  onExport: (id: string, title: string, e: React.MouseEvent) => void;
}) {
  return (
    <div
      className={`group relative mb-0.5 cursor-pointer rounded-lg transition-all
        ${isActive
          ? 'bg-[var(--bg-sidebar-active)] pl-[9px] border-l-[3px] border-l-[var(--accent)]'
          : 'border-l-[3px] border-l-transparent'}`}
      onClick={() => !isEditing && onSelect(id)}
      onMouseEnter={() => onHover(id)}
      onMouseLeave={() => onHover(null)}
    >
      <div className="flex items-center gap-1 py-2 pr-2">
        {loading ? (
          <div className="spinner shrink-0" />
        ) : (
          <MessageSquare size={14} className="shrink-0 text-[var(--text-tertiary)]" />
        )}

        {isEditing ? (
          <div className="flex min-w-0 flex-1 items-center gap-1">
            <input
              ref={editInputRef}
              value={editTitle}
              onChange={(e) => onEditTitleChange(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === 'Enter') onSaveEdit();
                if (e.key === 'Escape') onCancelEdit();
              }}
              onClick={(e) => e.stopPropagation()}
              className="min-w-0 flex-1 rounded border border-[var(--border-focus)] bg-[var(--bg-input)] px-1.5 py-0.5 text-[13px] text-[var(--text-primary)] outline-none"
              autoFocus
            />
            <button
              onClick={(e) => { e.stopPropagation(); onSaveEdit(); }}
              className="rounded p-0.5 text-[var(--color-success)]"
            >
              <Check size={13} />
            </button>
            <button
              onClick={(e) => { e.stopPropagation(); onCancelEdit(); }}
              className="rounded p-0.5 text-[var(--text-tertiary)]"
            >
              <X size={13} />
            </button>
          </div>
        ) : (
          <>
            <div className="min-w-0 flex-1">
              <div className={`truncate text-[13px] ${isActive ? 'font-medium' : ''} text-[var(--text-primary)]`}>
                {title}
              </div>
            </div>

            {(isHovered || isActive) && !loading && (
              <div className="flex shrink-0 items-center gap-0.5">
                <button
                  onClick={(e) => onExport(id, title, e)}
                  className="rounded p-1 text-[var(--text-tertiary)] transition-colors hover:text-[var(--text-primary)]"
                  title="导出"
                >
                  <Download size={12} />
                </button>
                <button
                  onClick={(e) => onStartEdit(id, e)}
                  className="rounded p-1 text-[var(--text-tertiary)] transition-colors hover:text-[var(--text-primary)]"
                  title="重命名"
                >
                  <Pencil size={12} />
                </button>
                <button
                  onClick={(e) => onDelete(id, e)}
                  className="rounded p-1 text-[var(--text-tertiary)] transition-colors hover:text-[var(--color-error)]"
                  title="删除"
                >
                  <Trash2 size={12} />
                </button>
              </div>
            )}
          </>
        )}
      </div>
    </div>
  );
}
