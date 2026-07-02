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
import type { WorkerTraceEvent, WorkerTraceState } from '../../stores/workerTraceStore';
import { useWorkerDetailStore } from '../../stores/workerDetailStore';
import { CacheUsageCard, cacheUsageForWorkers } from './TaskRuntimePanel';
import MarkdownContent from '../common/MarkdownContent';
import { InlineToolCall } from '../chat/InlineToolCall';
import { computeWorkerProgress, progressSummary, statusLabel } from '../../utils/workerProgress';

interface WorkerDetailViewProps {
  worker: WorkerTraceState;
  allWorkers: WorkerTraceState[];
  onBack: () => void;
}

interface WorkerStep {
  type: 'thinking' | 'tool' | 'text' | 'usage';
  content?: string;
  toolStart?: WorkerTraceEvent;
  toolResult?: WorkerTraceEvent;
  event?: WorkerTraceEvent;
}

function payloadRecord(event?: WorkerTraceEvent): Record<string, unknown> {
  return event?.payload && typeof event.payload === 'object' && !Array.isArray(event.payload)
    ? (event.payload as Record<string, unknown>)
    : {};
}

function payloadText(event: WorkerTraceEvent | undefined, ...keys: string[]): string {
  const payload = payloadRecord(event);
  for (const key of keys) {
    const value = payload[key];
    if (typeof value === 'string' && value.trim()) return value;
  }
  return '';
}

function toolName(event?: WorkerTraceEvent): string {
  return payloadText(event, 'name') || 'tool';
}

function reconstructSteps(events: WorkerTraceEvent[]): WorkerStep[] {
  const steps: WorkerStep[] = [];
  const thinking: string[] = [];
  const pendingTools: WorkerTraceEvent[] = [];

  const flushThinking = () => {
    const content = thinking.join('').trim();
    if (content) steps.push({ type: 'thinking', content });
    thinking.length = 0;
  };

  for (const event of events) {
    if (event.event_type === 'worker_thinking_delta') {
      const content = payloadText(event, 'content', 'text', 'delta');
      if (content) thinking.push(content);
      continue;
    }

    if (event.event_type === 'worker_thinking_end') {
      flushThinking();
      steps.push({ type: 'usage', event });
      continue;
    }

    if (event.event_type === 'worker_tool_start') {
      flushThinking();
      pendingTools.push(event);
      continue;
    }

    if (event.event_type === 'worker_tool_result') {
      flushThinking();
      const name = toolName(event);
      const idx = pendingTools.findIndex((candidate) => toolName(candidate) === name);
      const start = idx >= 0 ? pendingTools.splice(idx, 1)[0] : undefined;
      steps.push({ type: 'tool', toolStart: start, toolResult: event });
      continue;
    }

    if (event.event_type === 'worker_token_delta') {
      const content = payloadText(event, 'content', 'text', 'delta');
      if (content) steps.push({ type: 'text', content });
      continue;
    }

    if (event.event_type === 'worker_llm_usage') {
      steps.push({ type: 'usage', event });
    }
  }

  flushThinking();
  for (const start of pendingTools) steps.push({ type: 'tool', toolStart: start });
  return steps;
}

function workerResult(worker: WorkerTraceState): string {
  const terminal = [...worker.events]
    .reverse()
    .find(
      (event) => event.event_type === 'worker_completed' || event.event_type === 'worker_failed'
    );
  const fromTerminal = terminal
    ? payloadText(terminal, 'summary', 'output', 'text', 'content', 'error')
    : '';
  if (fromTerminal) return fromTerminal;

  return worker.events
    .filter((event) => event.event_type === 'worker_token_delta')
    .map((event) => payloadText(event, 'content', 'text', 'delta'))
    .join('')
    .trim();
}

function statusIcon(worker: WorkerTraceState) {
  if (worker.status === 'running') {
    return <Loader2 size={16} className="animate-spin" style={{ color: 'var(--color-info)' }} />;
  }
  if (worker.status === 'completed') {
    return <CheckCircle2 size={16} style={{ color: 'var(--color-success)' }} />;
  }
  if (worker.status === 'failed') {
    return <AlertCircle size={16} style={{ color: 'var(--color-error)' }} />;
  }
  return <Circle size={16} style={{ color: 'var(--text-tertiary)' }} />;
}

function formatTime(value?: string): string {
  if (!value) return 'unknown';
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return date.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', second: '2-digit' });
}

function usageLine(event: WorkerTraceEvent): string {
  const payload = payloadRecord(event);
  const prompt = Number(payload.prompt_tokens ?? 0);
  const completion = Number(payload.completion_tokens ?? 0);
  const cached = Number(payload.cached_prompt_tokens ?? 0);
  if (!prompt && !completion && !cached) return 'usage metadata';
  return `input ${prompt.toLocaleString()} / output ${completion.toLocaleString()} / cached ${cached.toLocaleString()}`;
}

export function WorkerDetailView({ worker, allWorkers, onBack }: WorkerDetailViewProps) {
  const [activeTab, setActiveTab] = useState<'process' | 'prompt' | 'result'>('process');
  const selectWorker = useWorkerDetailStore((state) => state.selectWorker);
  const progress = useMemo(() => computeWorkerProgress(worker), [worker.events, worker.status]);
  const steps = useMemo(() => reconstructSteps(worker.events), [worker.events]);
  const result = useMemo(() => workerResult(worker), [worker.events]);
  const childWorkers = useMemo(
    () => allWorkers.filter((candidate) => candidate.parentWorkerId === worker.workerId),
    [allWorkers, worker.workerId]
  );
  const cacheSummary = useMemo(
    () => cacheUsageForWorkers([worker, ...childWorkers]),
    [worker, childWorkers]
  );
  const title = worker.title || worker.agentName || worker.workerId;

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
              {statusIcon(worker)}
              <h2 className="truncate text-lg font-semibold text-[var(--text-primary)]">{title}</h2>
            </div>
            <div className="mt-1 flex flex-wrap gap-x-3 gap-y-1 text-xs text-[var(--text-tertiary)]">
              <span>{worker.agentName || 'subagent'}</span>
              <span>{statusLabel(progress.status)}</span>
              {progressSummary(progress) && <span>{progressSummary(progress)}</span>}
              <span>started {formatTime(worker.startedAt)}</span>
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
            {worker.task ? (
              <MarkdownContent content={worker.task} className="text-sm" />
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

                  const name = toolName(step.toolStart || step.toolResult);
                  const args = payloadRecord(step.toolStart).args ?? {};
                  const resultText = payloadText(step.toolResult, 'result');
                  const success = String(payloadRecord(step.toolResult).success) !== 'false';
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

            {childWorkers.length > 0 && (
              <div className="space-y-2 pt-2">
                <SectionTitle title="子 subagent" subtitle="由当前 subagent 派生的下级执行" />
                {childWorkers.map((child) => (
                  <button
                    key={child.workerId}
                    type="button"
                    className="flex w-full items-center gap-2 rounded-md px-3 py-2 text-left text-sm transition-colors hover:bg-[var(--bg-hover)]"
                    onClick={() => selectWorker(child.runId, child.workerId)}
                  >
                    {statusIcon(child)}
                    <span className="truncate text-[var(--text-primary)]">
                      {child.title || child.agentName || child.workerId}
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
