import { memo, useEffect, useRef, useState } from 'react';
import {
  AlertCircle,
  CheckCircle2,
  ChevronDown,
  ChevronRight,
  Circle,
  Loader2,
} from 'lucide-react';
import type { SubagentRunState } from '../../stores/subagentRunStore';
import { toolExecutionOwnerKey, useToolExecutionStore } from '../../stores/toolExecutionStore';
import MarkdownContent from '../common/MarkdownContent';
import { SubagentResultView } from '../subagent/SubagentResultView';
import { statusLabel } from '../../utils/subagentProgress';
import { subagentResultPresentation } from '../../utils/subagentResult';
import { InlineToolCall } from './InlineToolCall';

interface SubagentStreamBlockProps {
  run: SubagentRunState;
  taskTitle?: string;
}

type SubagentTab = 'task' | 'process' | 'result';

export const SubagentStreamBlock = memo(function SubagentStreamBlock({
  run,
  taskTitle,
}: SubagentStreamBlockProps) {
  const [expanded, setExpanded] = useState(run.status === 'running');
  const [activeTab, setActiveTab] = useState<SubagentTab>(
    run.status === 'running' ? 'process' : 'result'
  );
  const previousStatus = useRef(run.status);
  const userControlledExpansion = useRef(false);
  const ownerKey = toolExecutionOwnerKey({
    kind: 'subagent',
    subagent_run_id: run.subagentRunId,
  });
  const toolIds = useToolExecutionStore((state) => state.idsByOwner[ownerKey] ?? []);
  const summary = toolIds.length > 0 ? `${toolIds.length} 工具` : '';
  const presentation = subagentResultPresentation(run);

  useEffect(() => {
    if (previousStatus.current === 'running' && run.status !== 'running') {
      setActiveTab('result');
      if (!userControlledExpansion.current) setExpanded(false);
    }
    previousStatus.current = run.status;
  }, [run.status]);

  const toggleExpanded = () => {
    userControlledExpansion.current = true;
    setExpanded((value) => !value);
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
    <div className="my-0.5 border-l border-[var(--border-primary)] pl-3">
      <div className="flex w-full items-center gap-1.5 text-[12px]">
        <button
          type="button"
          onClick={toggleExpanded}
          className="shrink-0 text-[var(--text-tertiary)]"
          aria-label={expanded ? '折叠 subagent' : '展开 subagent'}
        >
          {expanded ? <ChevronDown size={10} /> : <ChevronRight size={10} />}
        </button>
        <button
          type="button"
          onClick={toggleExpanded}
          className="flex min-w-0 flex-1 items-center gap-1.5 px-1 py-0.5 text-left hover:bg-[var(--bg-hover)]"
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
        <div className="ml-4 mt-1.5 min-w-0">
          <div className="flex h-8 items-end gap-1 border-b border-[var(--border-primary)]">
            {(
              [
                ['task', '提示词 / 任务'],
                ['process', '执行过程'],
                ['result', '结果'],
              ] as const
            ).map(([id, label]) => (
              <button
                key={id}
                type="button"
                onClick={() => setActiveTab(id)}
                className="h-8 border-b-2 px-2 text-[10px] font-medium"
                style={{
                  borderColor: activeTab === id ? 'var(--accent)' : 'transparent',
                  color: activeTab === id ? 'var(--text-primary)' : 'var(--text-tertiary)',
                }}
              >
                {label}
              </button>
            ))}
          </div>

          <div className="min-w-0 py-2">
            {activeTab === 'task' &&
              (run.task ? (
                <MarkdownContent content={run.task} className="text-sm" />
              ) : (
                <div className="text-[11px] text-[var(--text-tertiary)]">暂无任务输入</div>
              ))}

            {activeTab === 'process' && (
              <div className="space-y-1">
                {toolIds.length === 0 ? (
                  <div className="text-[11px] text-[var(--text-tertiary)]">暂无工具执行</div>
                ) : (
                  toolIds.map((toolId, index) => (
                    <InlineToolCall key={toolId} toolId={toolId} index={index} />
                  ))
                )}
              </div>
            )}

            {activeTab === 'result' &&
              (run.result || presentation.text ? (
                <SubagentResultView result={run.result} content={presentation.text} />
              ) : (
                <div className="text-[11px] text-[var(--text-tertiary)]">
                  {run.status === 'running' ? '正在等待执行结果' : '暂无结果'}
                </div>
              ))}
          </div>
        </div>
      )}
    </div>
  );
});
