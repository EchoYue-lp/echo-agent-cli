import { useEffect } from 'react';
import { AlertCircle, X } from 'lucide-react';
import { useTaskRuntimeStore } from '../../stores/taskRuntimeStore';

interface FailureToastProps {
  /** Pass to dismiss manually (e.g. user clicks X or "查看"). */
  onDismiss: () => void;
}

/**
 * Appears as a toast bar above the chat input when the active run has failed
 * items (todos with failed status, or activeRun.status === 'failed').
 *
 * Auto-dismisses after 5 seconds. Shows count and a "查看" button that can
 * focus the right rail (external). No details — details are in the right rail.
 *
 * Spec §3.4: 执行失败 toast。
 */
export function FailureToast({ onDismiss }: FailureToastProps) {
  const activeRun = useTaskRuntimeStore((s) => s.activeRun);
  const todos = useTaskRuntimeStore((s) => s.todos);

  const failedTodos = todos.filter((t) => t.status === 'failed');
  const runFailed = activeRun?.status === 'failed';
  const count = failedTodos.length + (runFailed ? 1 : 0);

  // Auto-dismiss after 5s
  useEffect(() => {
    if (count === 0) return;
    const timer = window.setTimeout(onDismiss, 5000);
    return () => window.clearTimeout(timer);
  }, [count, onDismiss]);

  if (count === 0) return null;

  return (
    <div
      className="flex items-center gap-3 rounded-lg border px-3 py-2"
      style={{
        borderColor: 'var(--color-error)',
        background: 'var(--bg-secondary)',
      }}
    >
      <AlertCircle size={15} style={{ color: 'var(--color-error)' }} />
      <span className="flex-1 text-xs" style={{ color: 'var(--text-primary)' }}>
        {runFailed
          ? '任务执行失败'
          : `有 ${count} 项执行失败`}
      </span>
      <button
        onClick={onDismiss}
        className="flex items-center gap-1 rounded-md px-2 py-0.5 text-[11px] transition-colors"
        style={{ color: 'var(--text-secondary)' }}
      >
        <X size={12} />
      </button>
    </div>
  );
}
