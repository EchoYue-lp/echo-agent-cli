import { useMemo, useState } from 'react';
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
} from 'lucide-react';
import type { ExecutionEvent, SubagentRunState } from '../../stores/subagentRunStore';
import { useSubagentDetailStore } from '../../stores/subagentDetailStore';
import { CacheUsageCard, cacheUsageForRuns } from './TaskRuntimePanel';
import MarkdownContent from '../common/MarkdownContent';
import { InlineToolCall } from '../chat/InlineToolCall';
import {
  computeSubagentProgress,
  progressSummary,
  statusLabel,
} from '../../utils/subagentProgress';

interface SubagentDetailViewProps {
  run: SubagentRunState;
  allRuns: SubagentRunState[];
  onBack: () => void;
}

interface SubagentStep {
  type: 'thinking' | 'tool' | 'text' | 'usage';
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

    if (event.event === 'usage') {
      // thinking_ended maps to `usage`; flush accumulated thinking and emit a
      // usage step (carries token / cache diagnostics on the top level).
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

    if (event.event === 'token_delta') {
      if (event.content) steps.push({ type: 'text', content: event.content });
    }
  }

  flushThinking();
  for (const start of pendingTools) steps.push({ type: 'tool', toolStart: start });
  return steps;
}

function subagentResult(run: SubagentRunState): string {
  // SubagentRunState carries the final output directly (and error on failure).
  if (run.status === 'failed' && run.error) return run.error;
  if (run.output) return run.output;
  return run.events
    .filter((event) => event.event === 'token_delta')
    .map((event) => String(event.content ?? ''))
    .join('')
    .trim();
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
  const prompt = Number(event.prompt_tokens ?? 0);
  const completion = Number(event.completion_tokens ?? 0);
  const cached = Number(event.cached_prompt_tokens ?? 0);
  if (!prompt && !completion && !cached) return 'usage metadata';
  return `input ${prompt.toLocaleString()} / output ${completion.toLocaleString()} / cached ${cached.toLocaleString()}`;
}

export function SubagentDetailView({ run, allRuns, onBack }: SubagentDetailViewProps) {
  const [activeTab, setActiveTab] = useState<'process' | 'prompt' | 'result'>('process');
  const selectSubagent = useSubagentDetailStore((state) => state.selectSubagent);
  const progress = useMemo(() => computeSubagentProgress(run), [run.events, run.status]);
  const steps = useMemo(() => reconstructSteps(run.events), [run.events]);
  const result = useMemo(
    () => subagentResult(run),
    [run.events, run.output, run.error, run.status]
  );
  const childRuns = useMemo(
    () => allRuns.filter((candidate) => candidate.parent === run.subagentRunId),
    [allRuns, run.subagentRunId]
  );
  const cacheSummary = useMemo(() => cacheUsageForRuns([run, ...childRuns]), [run, childRuns]);
  const title = run.agent || run.subagentRunId;

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
            ['process', '执行过程', TerminalSquare],
            ['prompt', '提示词', ClipboardList],
            ['result', '结果', Gauge],
          ].map(([id, label, Icon]) => (
            <button
              key={id as string}
              type="button"
              onClick={() => setActiveTab(id as 'process' | 'prompt' | 'result')}
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
        {activeTab === 'prompt' && (
          <div className="mx-auto max-w-[880px]">
            <SectionTitle title="提示词" subtitle="这个 subagent 收到的任务输入" />
            {run.task ? (
              <MarkdownContent content={run.task} className="text-sm" />
            ) : (
              <EmptyState text="这个 subagent 暂时没有记录提示词。" />
            )}
          </div>
        )}

        {activeTab === 'result' && (
          <div className="mx-auto max-w-[880px]">
            <SectionTitle title="结果" subtitle="subagent 的最终摘要或输出流" />
            {result ? (
              <MarkdownContent content={result} className="text-sm" />
            ) : (
              <EmptyState text="还没有可展示的结果。" />
            )}
          </div>
        )}

        {activeTab === 'process' && (
          <div className="mx-auto max-w-[880px] space-y-4">
            <SectionTitle title="执行过程" subtitle="思考、工具调用、token/cache 事件" />
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
                  if (step.type === 'text') {
                    return (
                      <div key={index} className="text-sm text-[var(--text-secondary)]">
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
                  const args = step.toolStart?.args ?? {};
                  const resultText = String(step.toolResult?.result ?? '');
                  const success = step.toolResult?.success !== false;
                  return (
                    <InlineToolCall
                      key={index}
                      toolCall={{ name, args, result: resultText, success }}
                      index={index}
                    />
                  );
                })}
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

function EmptyState({ text }: { text: string }) {
  return (
    <div className="rounded-md border border-dashed border-[var(--border-primary)] px-4 py-8 text-center text-sm text-[var(--text-tertiary)]">
      {text}
    </div>
  );
}
