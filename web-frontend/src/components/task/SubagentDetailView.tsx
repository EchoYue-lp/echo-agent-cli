import { useEffect, useState } from 'react';
import {
  AlertCircle,
  ArrowLeft,
  CheckCircle2,
  Circle,
  ClipboardList,
  Gauge,
  Loader2,
  TerminalSquare,
} from 'lucide-react';
import type { SubagentRunState } from '../../stores/subagentRunStore';
import { toolExecutionOwnerKey, useToolExecutionStore } from '../../stores/toolExecutionStore';
import { statusLabel } from '../../utils/subagentProgress';
import { subagentResultPresentation } from '../../utils/subagentResult';
import MarkdownContent from '../common/MarkdownContent';
import { InlineToolCall } from '../chat/InlineToolCall';
import { SubagentResultView } from '../subagent/SubagentResultView';
import { CacheUsageCard, cacheUsageForRuns } from './TaskRuntimePanel';

interface SubagentDetailViewProps {
  run: SubagentRunState;
  onBack: () => void;
}

function statusIcon(run: SubagentRunState) {
  if (run.status === 'running') {
    return <Loader2 size={16} className="animate-spin text-[var(--color-info)]" />;
  }
  if (run.status === 'completed') {
    return <CheckCircle2 size={16} className="text-[var(--color-success)]" />;
  }
  if (run.status === 'failed' || run.status === 'timed_out') {
    return <AlertCircle size={16} className="text-[var(--color-error)]" />;
  }
  return <Circle size={16} className="text-[var(--text-tertiary)]" />;
}

function formatTime(epochMs: number): string {
  const date = new Date(epochMs);
  if (Number.isNaN(date.getTime())) return 'unknown';
  return date.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', second: '2-digit' });
}

export function SubagentDetailView({ run, onBack }: SubagentDetailViewProps) {
  const [activeTab, setActiveTab] = useState<'task' | 'process' | 'result'>(
    run.status === 'running' ? 'process' : 'result'
  );
  const presentation = subagentResultPresentation(run);
  const cacheSummary = cacheUsageForRuns([run]);
  const ownerKey = toolExecutionOwnerKey({
    kind: 'subagent',
    subagent_run_id: run.subagentRunId,
  });
  const toolIds = useToolExecutionStore((state) => state.idsByOwner[ownerKey] ?? []);

  useEffect(() => {
    if (run.status !== 'running') setActiveTab('result');
  }, [run.status]);

  return (
    <div className="flex h-full min-h-0 flex-col bg-[var(--bg-primary)]">
      <div className="shrink-0 border-b border-[var(--border-primary)] px-5 py-3 sm:px-8">
        <button
          type="button"
          onClick={onBack}
          className="mb-3 flex items-center gap-2 text-xs text-[var(--text-secondary)]"
        >
          <ArrowLeft size={14} />
          返回对话
        </button>

        <div className="flex min-w-0 items-start justify-between gap-4">
          <div className="min-w-0">
            <div className="flex items-center gap-2">
              {statusIcon(run)}
              <h2 className="truncate text-lg font-semibold text-[var(--text-primary)]">
                {run.agent || run.subagentRunId}
              </h2>
            </div>
            <div className="mt-1 flex flex-wrap gap-x-3 text-xs text-[var(--text-tertiary)]">
              <span>{statusLabel(run.status)}</span>
              {toolIds.length > 0 && <span>{toolIds.length} 工具</span>}
              <span>started {formatTime(run.startedAt)}</span>
            </div>
          </div>
          <div className="hidden w-72 shrink-0 md:block">
            <CacheUsageCard summary={cacheSummary} compact />
          </div>
        </div>

        <div className="mt-4 flex gap-1 border-b border-[var(--border-primary)]">
          {(
            [
              ['task', '提示词 / 任务', ClipboardList],
              ['process', '执行过程', TerminalSquare],
              ['result', '结果', Gauge],
            ] as const
          ).map(([id, label, Icon]) => (
            <button
              key={id}
              type="button"
              onClick={() => setActiveTab(id)}
              className="flex items-center gap-1.5 border-b-2 px-3 py-2 text-xs font-medium"
              style={{
                borderColor: activeTab === id ? 'var(--accent)' : 'transparent',
                color: activeTab === id ? 'var(--text-primary)' : 'var(--text-tertiary)',
              }}
            >
              <Icon size={13} />
              {label}
            </button>
          ))}
        </div>
      </div>

      <div className="min-h-0 flex-1 overflow-y-auto px-5 py-4 sm:px-8">
        {activeTab === 'task' && (
          <div className="mx-auto max-w-[880px]">
            <SectionTitle title="提示词 / 任务" subtitle="分派给 subagent 的完整任务输入" />
            {run.task ? (
              <MarkdownContent content={run.task} className="text-sm" />
            ) : (
              <EmptyState text="这个 subagent 暂时没有记录任务输入。" />
            )}
          </div>
        )}

        {activeTab === 'process' && (
          <div className="mx-auto max-w-[880px]">
            <SectionTitle title="执行过程" subtitle="工具调用按实际执行顺序排列" />
            {toolIds.length === 0 ? (
              <EmptyState text="暂无工具执行。" />
            ) : (
              <div className="space-y-1">
                {toolIds.map((toolId, index) => (
                  <InlineToolCall key={toolId} toolId={toolId} index={index} />
                ))}
              </div>
            )}
          </div>
        )}

        {activeTab === 'result' && (
          <div className="mx-auto max-w-[880px]">
            <SectionTitle title="结果" subtitle="subagent 返回的完整最终结果" />
            {run.result || presentation.text ? (
              <SubagentResultView result={run.result} content={presentation.text} />
            ) : (
              <EmptyState text="还没有可展示的结果。" />
            )}
          </div>
        )}
      </div>
    </div>
  );
}

function SectionTitle({ title, subtitle }: { title: string; subtitle: string }) {
  return (
    <div className="mb-4">
      <h3 className="text-sm font-semibold text-[var(--text-primary)]">{title}</h3>
      <p className="mt-1 text-xs text-[var(--text-tertiary)]">{subtitle}</p>
    </div>
  );
}

function EmptyState({ text }: { text: string }) {
  return <div className="py-8 text-center text-sm text-[var(--text-tertiary)]">{text}</div>;
}
