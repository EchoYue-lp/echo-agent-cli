import { AgentSentMessage } from './AgentSentMessage';
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
import { transcriptToolExecution, type TranscriptToolRecord } from '../../types/api';
import { InlineToolCall } from './InlineToolCall';

interface ToolExecutionGroupProps {
  toolIds: string[];
  transcriptTools?: readonly TranscriptToolRecord[];
  grouped?: boolean;
  /** 外层已提供折叠摘要时，直接显示原工具行，避免重复的摘要层。 */
  detailsOnly?: boolean;
}

export function toolExecutionGroupPresentation(
  toolIds: readonly string[],
  toolsById: ReturnType<typeof useToolExecutionStore.getState>['tools'],
  transcriptTools: readonly TranscriptToolRecord[] = []
) {
  const tools = toolIds.flatMap((toolId) => {
    const transcript = transcriptTools.find((record) => record.id === toolId);
    const tool = transcript ? transcriptToolExecution(transcript, toolsById) : toolsById[toolId];
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
  transcriptTools = [],
  grouped = false,
  detailsOnly = false,
}: ToolExecutionGroupProps) {
  const [expanded, setExpanded] = useState(false);
  const toolsById = useToolExecutionStore((state) => state.tools);
  const { runningCount, failedCount, cancelledCount, uncertainCount, missingCount, label } =
    toolExecutionGroupPresentation(toolIds, toolsById, transcriptTools);
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

  if (!grouped) {
    // Preserve original execution-step order while surfacing communication results outside collapsed tools.
    const communication = toolIds.some((id) => {
      const record = transcriptTools.find((row) => row.id === id);
      const tool = record ? transcriptToolExecution(record, toolsById) : toolsById[id];
      return tool != null && ['agent_message', 'agent_followup'].includes(tool.name);
    });
    if (communication)
      return (
        <>
          {toolIds.map((id) => {
            const record = transcriptTools.find((row) => row.id === id);
            const tool = record ? transcriptToolExecution(record, toolsById) : toolsById[id];
            return tool && ['agent_message', 'agent_followup'].includes(tool.name) ? (
              <AgentSentMessage key={id} tool={tool} transcript={record} />
            ) : (
              <ToolExecutionGroup
                key={id}
                toolIds={[id]}
                transcriptTools={transcriptTools}
                grouped
                detailsOnly={detailsOnly}
              />
            );
          })}
        </>
      );
  }
  const details = toolIds.map((toolId, index) => {
    const transcript = transcriptTools.find((record) => record.id === toolId);
    if (!toolsById[toolId] && !transcript)
      return (
        <div
          key={toolId}
          role="status"
          className="break-all py-1 text-xs text-[var(--text-tertiary)]"
        >
          工具记录 {toolId} · 详情未恢复
        </div>
      );
    return <InlineToolCall key={toolId} toolId={toolId} index={index} transcript={transcript} />;
  });
  if (detailsOnly) return <div className="min-w-0">{details}</div>;
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
        <div className="ml-3 border-l border-[var(--border-primary)] pl-2">{details}</div>
      )}
    </div>
  );
});
