import { useState, memo, useMemo } from 'react';
import { Loader2, CheckCircle2, AlertCircle, Circle, ChevronDown, ChevronRight, Brain } from 'lucide-react';
import type { WorkerTraceState, WorkerTraceEvent } from '../../stores/workerTraceStore';
import MarkdownContent from '../common/MarkdownContent';
import { InlineToolCall } from './InlineToolCall';
import { computeWorkerProgress, progressSummary, statusLabel } from '../../utils/workerProgress';

interface WorkerStreamBlockProps {
  worker: WorkerTraceState;
  /** All workers in this run (for recursive child lookup via parentWorkerId) */
  allWorkers: WorkerTraceState[];
}

/** Reconstruct worker's thinking+tool loop from raw events. */
interface WorkerStep {
  type: 'thinking' | 'tool';
  content?: string;
  toolStart?: WorkerTraceEvent;
  toolResult?: WorkerTraceEvent;
}

function reconstructSteps(events: WorkerTraceEvent[]): { steps: WorkerStep[]; thinkingTotal: number } {
  const steps: WorkerStep[] = [];
  let thinkingTotal = 0;
  const currentThinking: string[] = [];
  const pendingTools: WorkerTraceEvent[] = [];

  const flushThinking = () => {
    if (currentThinking.length > 0) {
      const content = currentThinking.join('').trim();
      if (content) {
        steps.push({ type: 'thinking', content });
        thinkingTotal++;
      }
      currentThinking.length = 0;
    }
  };

  for (const e of events) {
    if (e.event_type === 'worker_thinking_delta') {
      const c = String((e.payload as Record<string, unknown> | null)?.content ?? '');
      if (c) currentThinking.push(c);
    } else if (e.event_type === 'worker_thinking_end') {
      flushThinking();
    } else if (e.event_type === 'worker_tool_start') {
      flushThinking();
      pendingTools.push(e);
    } else if (e.event_type === 'worker_tool_result') {
      const name = String((e.payload as Record<string, unknown> | null)?.name ?? '');
      const idx = pendingTools.findIndex(
        (p) => String((p.payload as Record<string, unknown> | null)?.name ?? '') === name
      );
      const start = idx >= 0 ? pendingTools.splice(idx, 1)[0] : undefined;
      steps.push({ type: 'tool', toolStart: start, toolResult: e });
    }
  }
  flushThinking();
  for (const start of pendingTools) {
    steps.push({ type: 'tool', toolStart: start });
  }
  return { steps, thinkingTotal };
}

function workerResult(worker: WorkerTraceState): string {
  const completed = [...worker.events].reverse().find((e) => e.event_type === 'worker_completed');
  const summary = completed ? String((completed.payload as Record<string, unknown> | null)?.summary ?? '') : '';
  if (summary) return summary;
  return worker.events
    .filter((e) => e.event_type === 'worker_token_delta')
    .map((e) => String((e.payload as Record<string, unknown> | null)?.content ?? ''))
    .join('')
    .trim();
}

export const WorkerStreamBlock = memo(function WorkerStreamBlock({ worker, allWorkers }: WorkerStreamBlockProps) {
  const [expanded, setExpanded] = useState(worker.status === 'running');
  const [sectionExpanded, setSectionExpanded] = useState({
    prompt: false,
    process: true,
    result: true,
  });

  const progress = useMemo(() => computeWorkerProgress(worker), [worker.events, worker.status]);
  const summary = progressSummary(progress);
  const { steps } = useMemo(() => reconstructSteps(worker.events), [worker.events]);
  const result = useMemo(() => workerResult(worker), [worker.events]);

  const children = useMemo(
    () => allWorkers.filter((w) => w.parentWorkerId === worker.workerId),
    [allWorkers, worker.workerId]
  );

  const statusIcon =
    worker.status === 'running' ? (
      <Loader2 size={11} className="animate-spin" style={{ color: 'var(--color-info)' }} />
    ) : worker.status === 'completed' ? (
      <CheckCircle2 size={11} style={{ color: 'var(--color-success)' }} />
    ) : worker.status === 'failed' ? (
      <AlertCircle size={11} style={{ color: 'var(--color-error)' }} />
    ) : (
      <Circle size={11} style={{ color: 'var(--text-tertiary)' }} />
    );

  return (
    <div className="my-1 rounded-md border-l-2 border-[var(--color-purple)] bg-[var(--bg-primary)] px-3 py-1.5">
      {/* Header (always visible): title + status + progress summary */}
      <button
        onClick={() => setExpanded((e) => !e)}
        className="flex w-full items-center gap-1.5 text-left text-[11px]"
      >
        {expanded ? <ChevronDown size={10} className="text-[var(--text-tertiary)]" /> : <ChevronRight size={10} className="text-[var(--text-tertiary)]" />}
        {statusIcon}
        <span className="truncate font-medium text-[var(--text-primary)]">
          {worker.title || worker.agentName || worker.workerId}
        </span>
        <span className="ml-auto shrink-0 text-[10px] text-[var(--text-tertiary)]">
          {statusLabel(progress.status)}{summary ? ` · ${summary}` : ''}
        </span>
      </button>

      {expanded && (
        <div className="mt-1.5 space-y-1.5 pl-1">
          {/* Prompt */}
          {worker.task && (
            <div>
              <button
                onClick={() => setSectionExpanded((s) => ({ ...s, prompt: !s.prompt }))}
                className="flex items-center gap-1 text-[10px] font-medium text-[var(--text-tertiary)]"
              >
                {sectionExpanded.prompt ? <ChevronDown size={9} /> : <ChevronRight size={9} />}
                提示词
              </button>
              {sectionExpanded.prompt && (
                <MarkdownContent content={worker.task} className="text-sm" />
              )}
            </div>
          )}

          {/* Execution process: flat thinking + tool loop (NO nested left-border blocks) */}
          <div>
            <button
              onClick={() => setSectionExpanded((s) => ({ ...s, process: !s.process }))}
              className="flex items-center gap-1 text-[10px] font-medium text-[var(--text-tertiary)]"
            >
              {sectionExpanded.process ? <ChevronDown size={9} /> : <ChevronRight size={9} />}
              执行过程
            </button>
            {sectionExpanded.process && (
              <div className="mt-1 space-y-1">
                {steps.length === 0 && (
                  <div className="text-[11px] text-[var(--text-tertiary)]">暂无事件</div>
                )}
                {steps.map((step, i) => {
                  if (step.type === 'thinking') {
                    // Flat thinking: small label + markdown inline, no nested
                    // bordered box. Matches the one-stream layout where every
                    // item (thinking/tool/worker) is a peer in the flow.
                    return (
                      <div key={i}>
                        <div className="mb-0.5 flex items-center gap-1">
                          <Brain size={9} className="text-[var(--color-purple)]" />
                          <span className="text-[9px] font-medium text-[var(--color-purple)]">思考</span>
                        </div>
                        <MarkdownContent content={step.content || ''} className="text-sm" />
                      </div>
                    );
                  }
                  const name = String((step.toolStart?.payload as Record<string, unknown> | null)?.name ?? 'tool');
                  const args = (step.toolStart?.payload as Record<string, unknown> | null)?.args ?? {};
                  const resultStr = String((step.toolResult?.payload as Record<string, unknown> | null)?.result ?? '');
                  const success = String((step.toolResult?.payload as Record<string, unknown> | null)?.success) !== 'false';
                  return (
                    <InlineToolCall
                      key={i}
                      toolCall={{ name, args, result: resultStr, success }}
                      index={i}
                    />
                  );
                })}
                {/* Recursive children (nested sub-agents) */}
                {children.length > 0 && (
                  <div className="ml-2 border-l border-[var(--border-primary)] pl-2">
                    {children.map((child) => (
                      <WorkerStreamBlock key={child.workerId} worker={child} allWorkers={allWorkers} />
                    ))}
                  </div>
                )}
              </div>
            )}
          </div>

          {/* Result */}
          {result && (
            <div>
              <button
                onClick={() => setSectionExpanded((s) => ({ ...s, result: !s.result }))}
                className="flex items-center gap-1 text-[10px] font-medium text-[var(--text-tertiary)]"
              >
                {sectionExpanded.result ? <ChevronDown size={9} /> : <ChevronRight size={9} />}
                结果
              </button>
              {sectionExpanded.result && (
                <MarkdownContent content={result} className="text-sm" />
              )}
            </div>
          )}
        </div>
      )}
    </div>
  );
});
