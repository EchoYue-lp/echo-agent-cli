import { memo, useState } from 'react';
import {
  AlertTriangle,
  Check,
  ChevronDown,
  ChevronRight,
  CircleStop,
  LoaderCircle,
  Wrench,
} from 'lucide-react';
import { useToolExecutionStore } from '../../stores/toolExecutionStore';
import { InlineToolCall } from './InlineToolCall';

interface ToolExecutionGroupProps {
  toolIds: string[];
}

export function toolExecutionGroupPresentation(
  toolIds: readonly string[],
  toolsById: ReturnType<typeof useToolExecutionStore.getState>['tools']
) {
  const tools = toolIds.flatMap((toolId) => {
    const tool = toolsById[toolId];
    return tool ? [tool] : [];
  });
  const runningCount = tools.filter((tool) => tool.status === 'running').length;
  const failedCount = tools.filter(
    (tool) => tool.status === 'failed' || tool.status === 'timed_out'
  ).length;
  const cancelledCount = tools.filter((tool) => tool.status === 'cancelled').length;
  const uncertainCount = tools.filter(
    (tool) => tool.status === 'interrupted' || tool.status === 'unknown'
  ).length;
  const count = toolIds.length;
  const missingCount = Math.max(0, count - tools.length);
  const label =
    runningCount > 0
      ? `正在执行 ${count} 个工具`
      : missingCount > 0
        ? `${count} 个工具 · ${missingCount} 个状态未恢复`
        : `已执行 ${count} 个工具`;
  return {
    runningCount,
    failedCount,
    cancelledCount,
    uncertainCount,
    missingCount,
    count,
    label,
  };
}

export const ToolExecutionGroup = memo(function ToolExecutionGroup({
  toolIds,
}: ToolExecutionGroupProps) {
  const [expanded, setExpanded] = useState(false);
  const toolsById = useToolExecutionStore((state) => state.tools);
  const { runningCount, failedCount, cancelledCount, uncertainCount, missingCount, label } =
    toolExecutionGroupPresentation(toolIds, toolsById);
  const statusSuffix = [
    failedCount > 0 ? `${failedCount} 个失败` : '',
    cancelledCount > 0 ? `${cancelledCount} 个已取消` : '',
    uncertainCount > 0 ? `${uncertainCount} 个状态未知` : '',
  ]
    .filter(Boolean)
    .join(' · ');
  const statusIcon =
    runningCount > 0 ? (
      <LoaderCircle size={12} className="animate-spin text-[var(--accent)]" />
    ) : missingCount > 0 || uncertainCount > 0 ? (
      <AlertTriangle size={12} className="text-[var(--color-warning)]" />
    ) : failedCount > 0 ? (
      <AlertTriangle size={12} className="text-[var(--color-error)]" />
    ) : cancelledCount > 0 ? (
      <CircleStop size={12} className="text-[var(--text-tertiary)]" />
    ) : (
      <Check size={12} className="text-[var(--color-success)]" />
    );

  return (
    <div className="my-1 min-w-0 pl-2">
      <button
        type="button"
        onClick={() => setExpanded((value) => !value)}
        className="flex min-h-7 w-full min-w-0 items-center gap-1.5 rounded-md px-1.5 py-0.5 -ml-1.5 text-left text-[12px] text-[var(--text-tertiary)] transition-colors hover:bg-[var(--bg-hover)] hover:text-[var(--text-secondary)]"
        aria-expanded={expanded}
        aria-label={expanded ? '折叠工具执行过程' : '展开工具执行过程'}
      >
        {expanded ? <ChevronDown size={11} /> : <ChevronRight size={11} />}
        <span className="shrink-0">{statusIcon}</span>
        <Wrench size={12} className="shrink-0" />
        <span className="min-w-0 truncate">{label}</span>
        {statusSuffix && <span className="shrink-0">· {statusSuffix}</span>}
      </button>

      {expanded && (
        <div className="ml-3 border-l border-[var(--border-primary)] pl-2">
          {toolIds.map((toolId, index) => (
            <InlineToolCall key={toolId} toolId={toolId} index={index} />
          ))}
        </div>
      )}
    </div>
  );
});
