import { memo, useState } from 'react';
import {
  AlertCircle,
  CheckCircle2,
  ChevronDown,
  ChevronRight,
  Circle,
  Loader2,
} from 'lucide-react';
import type { SubagentRunState } from '../../stores/subagentRunStore';
import {
  toolExecutionIdsForOwner,
  toolExecutionOwnerKey,
  useToolExecutionStore,
} from '../../stores/toolExecutionStore';
import { useContextPaneStore } from '../../stores/contextPaneStore';
import { SubagentOutcomeView } from '../subagent/SubagentOutcomeView';
import { statusLabel } from '../../utils/subagentProgress';
import { subagentOutcomePresentation } from '../../utils/subagentOutcome';

interface SubagentStreamBlockProps {
  run: SubagentRunState;
  taskTitle?: string;
}

/**
 * One-line subagent row in the main chat stream (Claude Code Task-tool shape).
 *
 * The row itself is a status line; clicking it opens the full timeline —
 * task prompt + tool calls + result, rendered with the same components as the
 * main agent — in the right workspace panel. The chevron only toggles an
 * inline result peek, so the chat stream stays a flat timeline.
 */
export const SubagentStreamBlock = memo(function SubagentStreamBlock({
  run,
  taskTitle,
}: SubagentStreamBlockProps) {
  const [expanded, setExpanded] = useState(false);
  const ownerKey = toolExecutionOwnerKey(
    {
      kind: 'subagent',
      subagent_run_id: run.subagentRunId,
    },
    run.runId
  );
  const toolIds = useToolExecutionStore((state) =>
    toolExecutionIdsForOwner(state.idsByOwner, ownerKey)
  );
  const summary = toolIds.length > 0 ? `${toolIds.length} 工具` : '';
  const presentation = subagentOutcomePresentation(run);

  const openDetail = () => {
    useContextPaneStore.getState().openSubagent(run.runId, run.subagentRunId);
  };

  const statusIcon =
    run.status === 'running' ? (
      <Loader2 size={11} className="animate-spin text-[var(--color-info)]" />
    ) : run.status === 'completed' ? (
      <CheckCircle2 size={11} className="text-[var(--color-success)]" />
    ) : run.status === 'failed' || run.status === 'timed_out' ? (
      <AlertCircle size={11} className="text-[var(--color-error)]" />
    ) : (
      <Circle size={11} className="text-[var(--text-tertiary)]" />
    );

  return (
    <div className="my-0.5 min-w-0">
      <div className="flex w-full items-center gap-1.5 text-[12px]">
        <button
          type="button"
          onClick={() => setExpanded((value) => !value)}
          className="shrink-0 text-[var(--text-tertiary)]"
          aria-label={expanded ? '折叠结果摘要' : '展开结果摘要'}
        >
          {expanded ? <ChevronDown size={10} /> : <ChevronRight size={10} />}
        </button>
        <button
          type="button"
          onClick={openDetail}
          className="flex min-w-0 flex-1 items-center gap-1.5 rounded-md px-1.5 py-0.5 text-left hover:bg-[var(--bg-hover)]"
          title="在右侧边栏查看完整执行过程"
        >
          {statusIcon}
          <span className="shrink-0 font-medium text-[var(--text-primary)]">Subagent</span>
          <span className="shrink-0 text-[var(--text-secondary)]">{run.agent}</span>
          {taskTitle && <span className="truncate text-[var(--text-primary)]">{taskTitle}</span>}
          <span className="ml-auto shrink-0 text-[10px] text-[var(--text-tertiary)]">
            {statusLabel(run.status)}
            {summary ? ` · ${summary}` : ''}
          </span>
        </button>
      </div>

      {expanded && (
        <div className="ml-5 mt-1 min-w-0 pl-2">
          {run.outcome || presentation.text ? (
            <SubagentOutcomeView
              outcome={run.outcome}
              content={presentation.text}
              maxHeight={200}
            />
          ) : (
            <div className="flex items-center gap-1.5 py-0.5 text-[11px] text-[var(--text-tertiary)]">
              {run.status === 'running' && <Loader2 size={11} className="animate-spin" />}
              {run.status === 'running' ? '正在执行，点击标题行查看实时过程' : '暂无结果'}
            </div>
          )}
        </div>
      )}
    </div>
  );
});
