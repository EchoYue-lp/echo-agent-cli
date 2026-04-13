import { useEffect, useState, useRef } from 'react';
import { Plus, Sun, Moon, Trash2, MessageSquare, Pencil, Check, X, Search, Download } from 'lucide-react';
import { useChatStore } from '../../stores/chatStore';
import { useConversationStore, type ConversationGroup } from '../../stores/conversationStore';
import { useUiStore } from '../../stores/uiStore';

export function LeftSidebar() {
  const messages = useChatStore((s) => s.messages);
  const theme = useUiStore((s) => s.theme);
  const toggleTheme = useUiStore((s) => s.toggleTheme);

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

  // Init on mount
  useEffect(() => {
    init();
  }, [init]);

  // Auto-focus edit input
  useEffect(() => {
    if (editingId && editInputRef.current) {
      editInputRef.current.focus();
      editInputRef.current.select();
    }
  }, [editingId]);

  // Filter conversations by search query
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
    <div className="flex h-full flex-col" style={{ background: 'var(--bg-sidebar)' }}>
      {/* Header */}
      <div className="flex items-center justify-between px-3 py-3" style={{ borderBottom: '1px solid var(--border-primary)' }}>
        <button
          onClick={startNew}
          className="flex flex-1 items-center gap-2 rounded-lg px-3 py-2 text-sm font-medium transition-colors"
          style={{ border: '1px solid var(--border-primary)', color: 'var(--text-primary)' }}
          onMouseEnter={(e) => (e.currentTarget.style.background = 'var(--bg-sidebar-hover)')}
          onMouseLeave={(e) => (e.currentTarget.style.background = 'transparent')}
        >
          <Plus size={15} />
          New Chat
        </button>
        <button
          onClick={toggleTheme}
          className="ml-2 rounded-lg p-2 transition-colors"
          style={{ color: 'var(--text-secondary)' }}
          onMouseEnter={(e) => (e.currentTarget.style.background = 'var(--bg-sidebar-hover)')}
          onMouseLeave={(e) => (e.currentTarget.style.background = 'transparent')}
          title={theme === 'dark' ? 'Light mode' : 'Dark mode'}
        >
          {theme === 'dark' ? <Sun size={15} /> : <Moon size={15} />}
        </button>
      </div>

      {/* Search */}
      {conversations.length > 0 && (
        <div className="px-2 pt-2 pb-1">
          <div
            className="flex items-center gap-2 rounded-lg px-3 py-1.5"
            style={{ background: 'var(--bg-hover)', border: '1px solid var(--border-primary)' }}
          >
            <Search size={13} style={{ color: 'var(--text-tertiary)', flexShrink: 0 }} />
            <input
              ref={searchInputRef}
              value={searchQuery}
              onChange={(e) => setSearchQuery(e.target.value)}
              placeholder="Search conversations..."
              className="flex-1 bg-transparent text-xs outline-none"
              style={{ color: 'var(--text-primary)' }}
            />
            {searchQuery && (
              <button onClick={() => { setSearchQuery(''); searchInputRef.current?.focus(); }}
                className="shrink-0 rounded p-0.5"
                style={{ color: 'var(--text-tertiary)' }}
              >
                <X size={12} />
              </button>
            )}
          </div>
        </div>
      )}

      {/* Current Chat */}
      <div className="px-2 pt-1 pb-1">
        <button
          onClick={() => { if (!isCurrentChatActive) startNew(); }}
          className="flex w-full items-center gap-2.5 rounded-lg px-3 py-2.5 text-left transition-colors"
          style={{
            background: isCurrentChatActive ? 'var(--bg-sidebar-active)' : 'transparent',
            cursor: isCurrentChatActive ? 'default' : 'pointer',
          }}
          onMouseEnter={(e) => {
            if (!isCurrentChatActive) e.currentTarget.style.background = 'var(--bg-sidebar-hover)';
          }}
          onMouseLeave={(e) => {
            if (!isCurrentChatActive) e.currentTarget.style.background = 'transparent';
          }}
        >
          <MessageSquare size={15} style={{ color: 'var(--accent)', flexShrink: 0 }} />
          <div className="min-w-0 flex-1">
            <div className="truncate text-[13px] font-medium" style={{ color: 'var(--text-primary)' }}>
              Today
            </div>
            <div className="text-xs" style={{ color: 'var(--text-tertiary)' }}>
              {messages.length > 0 ? `${messages.length} messages` : 'Empty'}
            </div>
          </div>
          {isCurrentChatActive && (
            <div className="h-2 w-2 rounded-full" style={{ background: 'var(--accent)', flexShrink: 0 }} />
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
            <Search size={24} style={{ color: 'var(--text-tertiary)', margin: '0 auto 8px' }} />
            <p className="text-xs" style={{ color: 'var(--text-tertiary)' }}>
              No results for "{searchQuery}"
            </p>
          </div>
        )}

        {groups.length === 0 && !searchQuery && conversations.length === 0 && (
          <div className="px-3 py-8 text-center">
            <MessageSquare size={24} style={{ color: 'var(--text-tertiary)', margin: '0 auto 8px' }} />
            <p className="text-xs" style={{ color: 'var(--text-tertiary)' }}>
              No conversations yet
            </p>
          </div>
        )}
      </div>
    </div>
  );
}

// ── Group Section ──

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
      <div className="px-3 py-1.5 text-[11px] font-semibold uppercase tracking-wider" style={{ color: 'var(--text-tertiary)' }}>
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

// ── Single Conversation Item ──

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
      className="group relative mb-0.5 cursor-pointer rounded-lg transition-all"
      style={{
        background: isActive ? 'var(--bg-sidebar-active)' : 'transparent',
        paddingLeft: isActive ? '9px' : '12px',
        borderLeft: isActive ? '3px solid var(--accent)' : '3px solid transparent',
      }}
      onClick={() => !isEditing && onSelect(id)}
      onMouseEnter={() => onHover(id)}
      onMouseLeave={() => onHover(null)}
    >
      <div className="flex items-center gap-1 py-2 pr-2">
        {loading ? (
          <div className="spinner" style={{ flexShrink: 0 }} />
        ) : (
          <MessageSquare size={14} style={{ color: 'var(--text-tertiary)', flexShrink: 0 }} />
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
              className="min-w-0 flex-1 rounded px-1.5 py-0.5 text-[13px] outline-none"
              style={{
                background: 'var(--bg-input)',
                color: 'var(--text-primary)',
                border: '1px solid var(--border-focus)',
              }}
            />
            <button
              onClick={(e) => { e.stopPropagation(); onSaveEdit(); }}
              className="rounded p-0.5"
              style={{ color: '#16a34a' }}
            >
              <Check size={13} />
            </button>
            <button
              onClick={(e) => { e.stopPropagation(); onCancelEdit(); }}
              className="rounded p-0.5"
              style={{ color: 'var(--text-tertiary)' }}
            >
              <X size={13} />
            </button>
          </div>
        ) : (
          <>
            <div className="min-w-0 flex-1">
              <div className="truncate text-[13px]" style={{
                color: 'var(--text-primary)',
                fontWeight: isActive ? 500 : 400,
              }}>
                {title}
              </div>
            </div>

            {/* Actions on hover */}
            {(isHovered || isActive) && !loading && (
              <div className="flex shrink-0 items-center gap-0.5">
                <button
                  onClick={(e) => onExport(id, title, e)}
                  className="rounded p-1 transition-colors"
                  style={{ color: 'var(--text-tertiary)' }}
                  onMouseEnter={(e) => (e.currentTarget.style.color = 'var(--text-primary)')}
                  onMouseLeave={(e) => (e.currentTarget.style.color = 'var(--text-tertiary)')}
                  title="Export"
                >
                  <Download size={12} />
                </button>
                <button
                  onClick={(e) => onStartEdit(id, e)}
                  className="rounded p-1 transition-colors"
                  style={{ color: 'var(--text-tertiary)' }}
                  onMouseEnter={(e) => (e.currentTarget.style.color = 'var(--text-primary)')}
                  onMouseLeave={(e) => (e.currentTarget.style.color = 'var(--text-tertiary)')}
                  title="Rename"
                >
                  <Pencil size={12} />
                </button>
                <button
                  onClick={(e) => onDelete(id, e)}
                  className="rounded p-1 transition-colors"
                  style={{ color: 'var(--text-tertiary)' }}
                  onMouseEnter={(e) => (e.currentTarget.style.color = '#ef4444')}
                  onMouseLeave={(e) => (e.currentTarget.style.color = 'var(--text-tertiary)')}
                  title="Delete"
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
