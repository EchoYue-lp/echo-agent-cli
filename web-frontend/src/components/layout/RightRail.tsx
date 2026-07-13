import { useEffect, useMemo, useState } from 'react';
import {
  CheckCircle2,
  Circle,
  Loader2,
  AlertCircle,
  ListTodo,
  ChevronLeft,
  PanelRightClose,
} from 'lucide-react';
import { useConversationStore } from '../../stores/conversationStore';
import { useChangesStore } from '../../stores/changesStore';
import { useTaskRuntimeStore } from '../../stores/taskRuntimeStore';
import { deriveChangedFiles } from '../../utils/deriveChangedFiles';
import { useChatStore } from '../../stores/chatStore';
import { ChangesDrawer } from '../changes/ChangesDrawer';

const TODO_LABEL: Record<string, string> = {
  pending: '待处理',
  running: '执行中',
  blocked: '阻塞',
  completed: '已完成',
  failed: '失败',
  skipped: '已跳过',
};

/** Status icon for a todo, driven by the store's raw status (no subagent derivation). */
function TodoIcon({ status }: { status: string }) {
  switch (status) {
    case 'completed':
      return <CheckCircle2 size={13} style={{ color: 'var(--color-success)' }} />;
    case 'running':
      return <Loader2 size={13} className="animate-spin" style={{ color: 'var(--color-info)' }} />;
    case 'failed':
      return <AlertCircle size={13} style={{ color: 'var(--color-error)' }} />;
    default:
      return <Circle size={13} style={{ color: 'var(--text-tertiary)' }} />;
  }
}

/**
 * RightRail — a compact task-todo capsule.
 *
 * Collapsed (default): a small floating pill in the top-right corner showing
 * the live todo progress (e.g. "2/5"). Expanded: a narrow panel with just the
 * todo list. Everything else the old "工作台" showed (run header, cache usage,
 * subagents, changed-files output) was removed — those are available elsewhere
 * (main timeline / changes drawer) and made the rail a heavy 316px card.
 */
export function RightRail() {
  const [expanded, setExpanded] = useState(false);
  const activeId = useConversationStore((s) => s.activeId);
  const messages = useChatStore((s) => s.messages);
  const { todos } = useTaskRuntimeStore();

  // Changed-files derivation feeds the ChangesDrawer (opened from the main
  // panel); the rail itself no longer lists them.
  useEffect(() => {
    useChangesStore.getState().checkSessionChange(activeId);
  }, [activeId]);
  const toolCallCount = useMemo(() => {
    let n = 0;
    for (const m of messages) n += (m.toolCalls ?? []).length;
    return n;
  }, [messages]);
  useEffect(() => {
    useChangesStore.getState().setFiles(deriveChangedFiles(messages));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [toolCallCount]);

  const completedCount = todos.filter((t) => t.status === 'completed').length;
  const hasTodos = todos.length > 0;

  // No todos and nothing to track → don't render the rail at all.
  if (!hasTodos) {
    return <ChangesDrawer />;
  }

  return (
    <aside className="relative hidden h-full shrink-0 overflow-visible lg:block">
      {/* Collapsed: small floating capsule */}
      {!expanded && (
        <button
          onClick={() => setExpanded(true)}
          className="absolute right-3 top-14 z-30 flex items-center gap-1.5 rounded-full border border-[var(--border-primary)] bg-[var(--bg-primary)]/95 px-2.5 py-1.5 text-xs font-medium tabular-nums text-[var(--text-secondary)] shadow-[var(--shadow-sm)] backdrop-blur transition-colors hover:border-[var(--accent)] hover:text-[var(--text-primary)]"
          title="展开任务列表"
          aria-label="展开任务列表"
        >
          <ListTodo size={13} />
          <span>
            {completedCount}/{todos.length}
          </span>
          <ChevronLeft size={12} className="text-[var(--text-tertiary)]" />
        </button>
      )}

      {/* Expanded: narrow todo-only panel */}
      {expanded && (
        <div className="flex h-full w-[240px] flex-col border-l border-[var(--border-primary)] bg-[var(--bg-rail)]">
          <header className="flex shrink-0 items-center justify-between gap-2 px-3 py-2.5">
            <div className="flex min-w-0 items-center gap-1.5">
              <ListTodo size={13} style={{ color: 'var(--accent)' }} />
              <span className="text-xs font-medium text-[var(--text-primary)]">任务</span>
              <span className="text-[10px] tabular-nums text-[var(--text-tertiary)]">
                {completedCount}/{todos.length}
              </span>
            </div>
            <button
              onClick={() => setExpanded(false)}
              className="flex h-6 w-6 shrink-0 items-center justify-center rounded-md text-[var(--text-tertiary)] transition-colors hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]"
              title="收起"
              aria-label="收起"
            >
              <PanelRightClose size={14} />
            </button>
          </header>

          <div className="min-h-0 flex-1 space-y-0.5 overflow-y-auto px-2 pb-3">
            {todos.map((todo) => (
              <div
                key={todo.id}
                className="flex items-start gap-1.5 rounded-md px-1.5 py-1 hover:bg-[var(--bg-hover)]"
              >
                <div className="mt-0.5 shrink-0">
                  <TodoIcon status={todo.status} />
                </div>
                <div className="min-w-0 flex-1">
                  <div
                    className="truncate text-[11px] text-[var(--text-primary)]"
                    title={todo.title}
                  >
                    {todo.title}
                  </div>
                  <div className="text-[9px] text-[var(--text-tertiary)]">
                    {TODO_LABEL[todo.status] ?? todo.status}
                  </div>
                </div>
              </div>
            ))}
          </div>
        </div>
      )}

      <ChangesDrawer />
    </aside>
  );
}
