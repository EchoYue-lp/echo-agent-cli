import { useEffect } from 'react';
import { AlertCircle, CheckCircle2, Circle, FileDiff, ListTodo, Loader2 } from 'lucide-react';
import { useTaskRuntimeStore } from '../../stores/taskRuntimeStore';
import { useFileStore } from '../../stores/fileStore';
import { useRightWorkspaceStore } from '../../stores/rightWorkspaceStore';

const TODO_LABEL: Record<string, string> = {
  pending: '待处理',
  running: '执行中',
  blocked: '阻塞',
  completed: '已完成',
  failed: '失败',
  skipped: '已跳过',
};

function TodoIcon({ status }: { status: string }) {
  switch (status) {
    case 'completed':
      return <CheckCircle2 size={13} className="text-[var(--color-success)]" />;
    case 'running':
      return <Loader2 size={13} className="animate-spin text-[var(--color-info)]" />;
    case 'failed':
      return <AlertCircle size={13} className="text-[var(--color-error)]" />;
    default:
      return <Circle size={13} className="text-[var(--text-tertiary)]" />;
  }
}

export function RightRail() {
  const todos = useTaskRuntimeStore((state) => state.todos);
  const changes = useFileStore((state) => state.changes);
  const loadChanges = useFileStore((state) => state.loadChanges);
  const loadDiff = useFileStore((state) => state.loadDiff);
  const openFiles = useRightWorkspaceStore((state) => state.openFiles);
  const completed = todos.filter((todo) => todo.status === 'completed').length;

  useEffect(() => {
    void loadChanges();
    const timer = window.setInterval(() => void loadChanges(), 2500);
    return () => window.clearInterval(timer);
  }, [loadChanges]);

  return (
    <div className="flex h-full min-h-0 flex-col overflow-y-auto">
      <section className="border-b border-[var(--border-primary)]">
        <div className="flex h-9 items-center justify-between px-3">
          <div className="flex items-center gap-1.5 text-xs font-medium text-[var(--text-primary)]">
            <ListTodo size={13} className="text-[var(--accent)]" />
            任务
          </div>
          <span className="text-[10px] tabular-nums text-[var(--text-tertiary)]">
            {completed}/{todos.length}
          </span>
        </div>
        <div className="space-y-0.5 px-2 pb-3">
          {todos.length === 0 ? (
            <div className="px-2 py-5 text-center text-xs text-[var(--text-tertiary)]">
              当前没有执行中的任务
            </div>
          ) : (
            todos.map((todo) => (
              <div
                key={todo.id}
                className="flex items-start gap-2 rounded-md px-2 py-1.5 hover:bg-[var(--bg-hover)]"
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
            ))
          )}
        </div>
      </section>

      <section className="min-h-0 flex-1">
        <div className="flex h-9 items-center justify-between px-3">
          <div className="flex items-center gap-1.5 text-xs font-medium text-[var(--text-primary)]">
            <FileDiff size={13} className="text-[var(--accent)]" />
            工作区变更
          </div>
          <span className="text-[10px] text-[var(--text-tertiary)]">{changes.length}</span>
        </div>
        <div className="space-y-0.5 px-2 pb-3">
          {changes.length === 0 ? (
            <div className="px-2 py-5 text-center text-xs text-[var(--text-tertiary)]">
              工作区没有未提交变更
            </div>
          ) : (
            changes.map((change) => (
              <button
                key={`${change.status}:${change.path}`}
                type="button"
                onClick={() => {
                  openFiles();
                  void loadDiff(change.path);
                }}
                className="flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left hover:bg-[var(--bg-hover)]"
              >
                <span
                  className={`h-1.5 w-1.5 shrink-0 rounded-full ${
                    change.status === 'added'
                      ? 'bg-[var(--color-success)]'
                      : change.status === 'deleted'
                        ? 'bg-[var(--color-error)]'
                        : 'bg-[var(--color-warning)]'
                  }`}
                />
                <span className="min-w-0 flex-1 truncate font-mono text-[10px] text-[var(--text-secondary)]">
                  {change.path}
                </span>
              </button>
            ))
          )}
        </div>
      </section>
    </div>
  );
}
