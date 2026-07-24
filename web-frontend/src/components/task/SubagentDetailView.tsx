import { useEffect, useMemo, useState } from 'react';
import {
  ArrowLeft,
  Brain,
  CheckCircle2,
  Circle,
  Loader2,
  AlertCircle,
  TerminalSquare,
  Gauge,
  ClipboardList,
  Files,
} from 'lucide-react';
import type { ExecutionEvent, SubagentRunState } from '../../stores/subagentRunStore';
import { useSubagentDetailStore } from '../../stores/subagentDetailStore';
import { CacheUsageCard, cacheUsageForRuns } from './TaskRuntimePanel';
import MarkdownContent from '../common/MarkdownContent';
import { InlineToolCall } from '../chat/InlineToolCall';
import { isSubagentDispatchTool } from '../chat/tools/toolRenderers';
import {
  computeSubagentProgress,
  progressSummary,
  statusLabel,
} from '../../utils/subagentProgress';
import { isCanonicalUsageEvent } from '../compress/subagentUsage';
import { SubagentResultView } from '../subagent/SubagentResultView';
import { subagentResultPresentation, withoutPromotedThinking } from '../../utils/subagentResult';

interface SubagentDetailViewProps {
  run: SubagentRunState;
  allRuns: SubagentRunState[];
  onBack: () => void;
}

interface SubagentStep {
  type: 'thinking' | 'tool' | 'usage';
  content?: string;
  toolStart?: ExecutionEvent;
  toolResult?: ExecutionEvent;
  event?: ExecutionEvent;
}

function reconstructSteps(events: ExecutionEvent[]): SubagentStep[] {
  const steps: SubagentStep[] = [];
  const thinking: string[] = [];
  const pendingTools: ExecutionEvent[] = [];

  const flushThinking = () => {
    const content = thinking.join('').trim();
    if (content) steps.push({ type: 'thinking', content });
    thinking.length = 0;
  };

  for (const event of events) {
    if (event.event === 'thinking_delta') {
      if (event.content) thinking.push(event.content);
      continue;
    }

    if (event.event === 'thinking_ended') {
      flushThinking();
      continue;
    }

    if (isCanonicalUsageEvent(event)) {
      flushThinking();
      steps.push({ type: 'usage', event });
      continue;
    }

    if (event.event === 'tool_started') {
      flushThinking();
      pendingTools.push(event);
      continue;
    }

    if (event.event === 'tool_completed') {
      flushThinking();
      const name = String(event.name ?? '');
      const idx = pendingTools.findIndex((c) => String(c.name ?? '') === name);
      const start = idx >= 0 ? pendingTools.splice(idx, 1)[0] : undefined;
      steps.push({ type: 'tool', toolStart: start, toolResult: event });
      continue;
    }
  }

  flushThinking();
  for (const start of pendingTools) steps.push({ type: 'tool', toolStart: start });
  return steps;
}

function statusIcon(run: SubagentRunState) {
  if (run.status === 'running') {
    return <Loader2 size={16} className="animate-spin" style={{ color: 'var(--color-info)' }} />;
  }
  if (run.status === 'completed') {
    return <CheckCircle2 size={16} style={{ color: 'var(--color-success)' }} />;
  }
  if (run.status === 'failed') {
    return <AlertCircle size={16} style={{ color: 'var(--color-error)' }} />;
  }
  return <Circle size={16} style={{ color: 'var(--text-tertiary)' }} />;
}

function formatTime(epochMs: number): string {
  const date = new Date(epochMs);
  if (Number.isNaN(date.getTime())) return 'unknown';
  return date.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', second: '2-digit' });
}

function usageLine(event: ExecutionEvent): string {
  const parts: string[] = [];
  if (typeof event.prompt_tokens === 'number') {
    parts.push(`input ${event.prompt_tokens.toLocaleString()}`);
  }
  if (typeof event.completion_tokens === 'number') {
    parts.push(`output ${event.completion_tokens.toLocaleString()}`);
  }
  if (typeof event.cached_prompt_tokens === 'number') {
    parts.push(`cached ${event.cached_prompt_tokens.toLocaleString()}`);
  }
  return parts.length > 0 ? parts.join(' / ') : 'usage metadata';
}

export function SubagentDetailView({ run, allRuns, onBack }: SubagentDetailViewProps) {
  const [activeTab, setActiveTab] = useState<'task' | 'process' | 'result'>(
    run.status === 'running' ? 'process' : 'result'
  );
  const selectSubagent = useSubagentDetailStore((state) => state.selectSubagent);
  const progress = useMemo(() => computeSubagentProgress(run), [run.events, run.status]);
  const presentation = useMemo(() => subagentResultPresentation(run), [run]);
  const steps = useMemo(
    () => withoutPromotedThinking(reconstructSteps(run.events), presentation.promotedThinking),
    [presentation.promotedThinking, run.events]
  );
  const childRuns = useMemo(
    () => allRuns.filter((candidate) => candidate.parent === run.subagentRunId),
    [allRuns, run.subagentRunId]
  );
  const cacheSummary = useMemo(() => cacheUsageForRuns([run, ...childRuns]), [run, childRuns]);
  const title = run.agent || run.subagentRunId;

  useEffect(() => {
    if (run.status !== 'running') setActiveTab('result');
  }, [run.status]);

  return (
    <div className="flex h-full min-h-0 flex-col bg-[var(--bg-primary)]">
      <div className="shrink-0 border-b border-[var(--border-primary)] px-5 py-3 sm:px-8">
        <button
          type="button"
          onClick={onBack}
          className="mb-3 flex items-center gap-2 text-xs text-[var(--text-secondary)] transition-colors hover:text-[var(--text-primary)]"
        >
          <ArrowLeft size={14} />
          返回对话
        </button>

        <div className="flex min-w-0 items-start justify-between gap-4">
          <div className="min-w-0">
            <div className="flex min-w-0 items-center gap-2">
              {statusIcon(run)}
              <h2 className="truncate text-lg font-semibold text-[var(--text-primary)]">{title}</h2>
            </div>
            <div className="mt-1 flex flex-wrap gap-x-3 gap-y-1 text-xs text-[var(--text-tertiary)]">
              <span>{run.agent || 'subagent'}</span>
              <span>{statusLabel(progress.status)}</span>
              {progressSummary(progress) && <span>{progressSummary(progress)}</span>}
              <span>started {formatTime(run.startedAt)}</span>
            </div>
          </div>
          <div className="hidden w-72 shrink-0 md:block">
            <CacheUsageCard summary={cacheSummary} compact />
          </div>
        </div>

        <div className="mt-4 flex gap-1 border-b border-[var(--border-primary)]">
          {[
            ['task', '提示词 / 任务', ClipboardList],
            ['process', '执行细节', TerminalSquare],
            ['result', '结果', Gauge],
          ].map(([id, label, Icon]) => (
            <button
              key={id as string}
              type="button"
              onClick={() => setActiveTab(id as 'process' | 'task' | 'result')}
              className="flex items-center gap-1.5 border-b-2 px-3 py-2 text-xs font-medium transition-colors"
              style={{
                borderColor: activeTab === id ? 'var(--accent)' : 'transparent',
                color: activeTab === id ? 'var(--text-primary)' : 'var(--text-tertiary)',
              }}
            >
              <Icon size={13} />
              {label as string}
            </button>
          ))}
        </div>
      </div>

      <div className="min-h-0 flex-1 overflow-y-auto px-5 py-4 sm:px-8">
        {activeTab === 'task' && (
          <div className="mx-auto max-w-[880px]">
            <SectionTitle title="提示词 / 任务" subtitle="分派给 subagent 的任务与上下文" />
            <div className="mb-3 flex flex-wrap gap-2 text-[10px]">
              <ContextChip label="prompt" value={run.promptSource ?? 'unknown'} />
              <ContextChip
                label="isolation requested"
                value={run.isolationRequested ?? 'unknown'}
              />
              <ContextChip label="isolation observed" value={run.isolationObserved ?? 'unknown'} />
              <ContextChip label="context" value={run.contextIn ?? 'unknown'} />
              <ContextChip label="return" value={run.returns ?? 'unknown'} />
            </div>
            {run.task ? (
              <MarkdownContent content={run.task} className="text-sm" />
            ) : (
              <EmptyState text="这个 subagent 暂时没有记录任务输入。" />
            )}
          </div>
        )}

        {activeTab === 'result' && (
          <div className="mx-auto max-w-[880px]">
            <SectionTitle title="结果" subtitle="subagent 的最终摘要或输出流" />
            {run.result || presentation.text ? (
              <SubagentResultView result={run.result} content={presentation.text} />
            ) : (
              <EmptyState text="还没有可展示的结果。" />
            )}
          </div>
        )}

        {activeTab === 'process' && (
          <div className="mx-auto max-w-[880px] space-y-4">
            <SectionTitle title="执行细节" subtitle="思考、工具调用、token/cache 事件" />
            <div className="md:hidden">
              <CacheUsageCard summary={cacheSummary} compact />
            </div>
            {steps.length === 0 ? (
              <EmptyState text="暂无执行事件。" />
            ) : (
              <div className="space-y-3">
                {steps.map((step, index) => {
                  if (step.type === 'thinking') {
                    return (
                      <div key={index} className="space-y-1">
                        <div className="flex items-center gap-1.5 text-xs font-medium text-[var(--color-purple)]">
                          <Brain size={13} />
                          思考
                        </div>
                        <MarkdownContent content={step.content || ''} className="text-sm" />
                      </div>
                    );
                  }
                  if (step.type === 'usage' && step.event) {
                    return (
                      <div
                        key={index}
                        className="flex items-center gap-2 text-xs text-[var(--text-tertiary)]"
                      >
                        <Gauge size={13} />
                        <span>{usageLine(step.event)}</span>
                      </div>
                    );
                  }

                  const name = String(step.toolStart?.name ?? step.toolResult?.name ?? 'tool');
                  if (isSubagentDispatchTool(name)) return null;
                  const args = step.toolStart?.args ?? {};
                  const resultText = String(step.toolResult?.result ?? '');
                  const success = step.toolResult?.success !== false;
                  return (
                    <InlineToolCall
                      key={index}
                      toolCall={{
                        id: String(step.toolStart?.call_id ?? `subagent-tool-${index}`),
                        name,
                        args,
                        result: resultText,
                        success,
                        status: step.toolResult ? (success ? 'succeeded' : 'failed') : 'running',
                        stdout: success ? resultText : '',
                        stderr: success ? '' : resultText,
                        log: '',
                        startedAt: Number(step.toolStart?.timestamp ?? Date.now()),
                        finishedAt: step.toolResult
                          ? Number(step.toolResult.timestamp ?? Date.now())
                          : undefined,
                      }}
                      index={index}
                    />
                  );
                })}
              </div>
            )}

            {run.result &&
              (run.result.touched_files.read.length > 0 ||
                run.result.touched_files.written.length > 0) && (
                <div className="space-y-1.5 border-t border-[var(--border-primary)] pt-3">
                  <div className="flex items-center gap-1.5 text-xs font-medium text-[var(--text-tertiary)]">
                    <Files size={13} />
                    文件访问
                  </div>
                  {run.result.touched_files.written.map((path) => (
                    <div key={`written-${path}`} className="break-all font-mono text-[10px]">
                      written · {path}
                    </div>
                  ))}
                  {run.result.touched_files.read.map((path) => (
                    <div key={`read-${path}`} className="break-all font-mono text-[10px]">
                      read · {path}
                    </div>
                  ))}
                </div>
              )}

            {childRuns.length > 0 && (
              <div className="space-y-2 pt-2">
                <SectionTitle title="子 subagent" subtitle="由当前 subagent 派生的下级执行" />
                {childRuns.map((child) => (
                  <button
                    key={child.subagentRunId}
                    type="button"
                    className="flex w-full items-center gap-2 rounded-md px-3 py-2 text-left text-sm transition-colors hover:bg-[var(--bg-hover)]"
                    onClick={() => selectSubagent(child.subagentRunId)}
                  >
                    {statusIcon(child)}
                    <span className="truncate text-[var(--text-primary)]">
                      {child.agent || child.subagentRunId}
                    </span>
                  </button>
                ))}
              </div>
            )}
          </div>
        )}
      </div>
    </div>
  );
}

function SectionTitle({ title, subtitle }: { title: string; subtitle: string }) {
  return (
    <div className="mb-3">
      <h3 className="text-sm font-semibold text-[var(--text-primary)]">{title}</h3>
      <p className="mt-0.5 text-xs text-[var(--text-tertiary)]">{subtitle}</p>
    </div>
  );
}

function ContextChip({ label, value }: { label: string; value: string }) {
  return (
    <span
      className="inline-flex max-w-full items-center gap-1 rounded-md border px-2 py-1"
      style={{
        borderColor: 'var(--border-primary)',
        background: 'var(--bg-secondary)',
        color: 'var(--text-tertiary)',
      }}
    >
      <span className="font-mono uppercase">{label}</span>
      <span className="truncate text-[var(--text-secondary)]">{value}</span>
    </span>
  );
}

function EmptyState({ text }: { text: string }) {
  return (
    <div className="rounded-md border border-dashed border-[var(--border-primary)] px-4 py-8 text-center text-sm text-[var(--text-tertiary)]">
      {text}
    </div>
  );
}
