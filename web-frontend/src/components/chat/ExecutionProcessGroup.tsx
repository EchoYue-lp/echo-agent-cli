import { memo, type ReactNode, useEffect, useState } from 'react';
import { Check, ChevronDown, ChevronRight, ListTree } from 'lucide-react';

interface ExecutionProcessGroupProps {
  completed: boolean;
  children: ReactNode;
}

/** Keep the live timeline visible, then collapse the whole process on completion. */
export const ExecutionProcessGroup = memo(function ExecutionProcessGroup({
  completed,
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
        <Check size={12} className="shrink-0 text-[var(--color-success)]" />
        <ListTree size={12} className="shrink-0" />
        <span>执行过程</span>
        <span className="text-[10px]">已完成</span>
      </button>

      {completedExpanded && <div className="ml-2 mt-1 min-w-0">{children}</div>}
    </div>
  );
});
