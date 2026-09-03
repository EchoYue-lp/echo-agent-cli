import { useEffect, useMemo, useState } from 'react';
import { Archive, ArchiveRestore, Loader2, Trash2 } from 'lucide-react';
import { useConversationStore } from '../../stores/conversationStore';
import { useToastStore } from '../../stores/toastStore';

/** Manage archived conversations separately from the active conversation list. */
export function ArchivedConversationsPanel() {
  const workspaceId = useConversationStore((state) => state.workspaceId);
  const conversations = useConversationStore((state) => state.conversations);
  const init = useConversationStore((state) => state.init);
  const restoreConversation = useConversationStore((state) => state.restoreConversation);
  const deleteConversation = useConversationStore((state) => state.deleteConversation);
  const addToast = useToastStore((state) => state.addToast);
  const [loading, setLoading] = useState(false);

  useEffect(() => {
    void init(workspaceId);
  }, [init, workspaceId]);

  const archived = useMemo(
    () => conversations.filter((conversation) => conversation.archived),
    [conversations]
  );

  const restore = async (id: string) => {
    setLoading(true);
    try {
      await restoreConversation(id);
      addToast('success', '会话已恢复');
    } catch (error) {
      console.error('Restore conversation failed:', error);
      addToast('error', '恢复会话失败，请重试');
    } finally {
      setLoading(false);
    }
  };

  const remove = async (id: string) => {
    if (!window.confirm('确定永久删除此会话？所有消息和运行记录将被清除。')) return;
    setLoading(true);
    try {
      await deleteConversation(id);
    } catch (error) {
      console.error('Delete conversation failed:', error);
      addToast('error', '删除会话失败，请重试');
    } finally {
      setLoading(false);
    }
  };

  return (
    <div className="space-y-4">
      <div className="flex items-start justify-between gap-4">
        <div>
          <h3 className="text-sm font-semibold text-[var(--text-primary)]">归档会话</h3>
          <p className="mt-1 text-xs text-[var(--text-tertiary)]">
            归档会话不会出现在侧边栏，可在这里恢复或永久删除。
          </p>
        </div>
        <Archive size={18} className="shrink-0 text-[var(--text-tertiary)]" />
      </div>

      {loading && archived.length === 0 && (
        <div className="flex items-center justify-center gap-2 py-10 text-xs text-[var(--text-tertiary)]">
          <Loader2 size={14} className="animate-spin" />
          加载中...
        </div>
      )}

      {!loading && archived.length === 0 && (
        <div className="border border-dashed border-[var(--border-primary)] px-4 py-10 text-center text-xs text-[var(--text-tertiary)]">
          暂无归档会话
        </div>
      )}

      {archived.length > 0 && (
        <div className="divide-y divide-[var(--border-primary)] border border-[var(--border-primary)]">
          {archived.map((conversation) => (
            <div key={conversation.id} className="flex items-center gap-3 px-3 py-3">
              <Archive size={14} className="shrink-0 text-[var(--text-tertiary)]" />
              <div className="min-w-0 flex-1">
                <div className="truncate text-sm text-[var(--text-primary)]">
                  {conversation.title || '新对话'}
                </div>
                <div className="mt-0.5 text-[11px] text-[var(--text-tertiary)]">
                  {conversation.messageCount} 条消息 ·{' '}
                  {new Date(conversation.updatedAt).toLocaleString()}
                </div>
              </div>
              <div className="flex shrink-0 items-center gap-1">
                <button
                  type="button"
                  disabled={loading}
                  onClick={() => void restore(conversation.id)}
                  className="flex items-center gap-1 rounded-md px-2 py-1.5 text-xs text-[var(--text-secondary)] hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)] disabled:opacity-50"
                  title="恢复会话"
                >
                  <ArchiveRestore size={13} />
                  恢复
                </button>
                <button
                  type="button"
                  disabled={loading}
                  onClick={() => void remove(conversation.id)}
                  className="flex items-center gap-1 rounded-md px-2 py-1.5 text-xs text-[var(--color-error)] hover:bg-red-500/10 disabled:opacity-50"
                  title="永久删除会话"
                >
                  <Trash2 size={13} />
                  永久删除
                </button>
              </div>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
