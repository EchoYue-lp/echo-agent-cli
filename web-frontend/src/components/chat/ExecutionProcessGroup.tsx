import { memo, type ReactNode, useEffect, useState } from 'react';
import { AlertTriangle, Check, ChevronDown, ChevronRight, ListTree } from 'lucide-react';

interface ExecutionProcessGroupProps {
  completed: boolean;
  /** Display knowledge only; it never changes the owning TaskRun lifecycle. */
  unrestored?: boolean;
  /** Known failed tool executions remain visible when the process is collapsed. */
  toolFailureCount?: number;
  children: ReactNode;
}

/** Keep the live timeline visible, then collapse the whole process on completion. */
export const ExecutionProcessGroup = memo(function ExecutionProcessGroup({
  completed,
  unrestored = false,
  toolFailureCount = 0,
  children,
}: ExecutionProcessGroupProps) {
  const [completedExpanded, setCompletedExpanded] = useState(false);

  useEffect(() => {
    if (completed) setCompletedExpanded(false);
  }, [completed]);

  if (!completed) return <>{children}</>;

  return (
    <div className="my-1 min-w-0 border-l border-[var(--border-primary)] pl-3">
      <button
        type="button"
        onClick={() => setCompletedExpanded((value) => !value)}
        className="flex min-h-7 w-full min-w-0 items-center gap-1.5 py-0.5 text-left text-[12px] text-[var(--text-tertiary)]"
        aria-expanded={completedExpanded}
        aria-label={completedExpanded ? '折叠执行过程' : '展开执行过程'}
      >
        {completedExpanded ? <ChevronDown size={11} /> : <ChevronRight size={11} />}
        {unrestored || toolFailureCount > 0 ? (
          <AlertTriangle
            size={12}
            className={`shrink-0 ${toolFailureCount > 0 ? 'text-[var(--color-error)]' : 'text-[var(--color-warning)]'}`}
          />
        ) : (
          <Check size={12} className="shrink-0 text-[var(--color-success)]" />
        )}
        <ListTree size={12} className="shrink-0" />
        <span>{unrestored ? '历史执行状态未恢复' : '执行过程'}</span>
        {!unrestored && <span className="text-[10px]">已完成</span>}
        {toolFailureCount > 0 && (
          <span className="text-[10px] text-[var(--color-error)]">
            {toolFailureCount} 个工具失败
          </span>
        )}
      </button>

      {completedExpanded && <div className="ml-2 mt-1 min-w-0">{children}</div>}
    </div>
  );
});
