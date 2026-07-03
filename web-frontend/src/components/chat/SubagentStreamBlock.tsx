import { useState, memo, useMemo } from 'react';
import {
  Loader2,
  CheckCircle2,
  AlertCircle,
  Circle,
  ChevronDown,
  ChevronRight,
  Brain,
} from 'lucide-react';
import type { SubagentRunState, ExecutionEvent } from '../../stores/subagentRunStore';
import { useSubagentDetailStore } from '../../stores/subagentDetailStore';
import MarkdownContent from '../common/MarkdownContent';
import { InlineToolCall } from './InlineToolCall';
import {
  computeSubagentProgress,
  progressSummary,
  statusLabel,
} from '../../utils/subagentProgress';

interface SubagentStreamBlockProps {
  run: SubagentRunState;
  /** All execution traces in this run (for recursive child lookup). */
  allRuns: SubagentRunState[];
}

/** Reconstruct a subagent's thinking+tool loop from raw events. */
interface SubagentStep {
  type: 'thinking' | 'tool';
  content?: string;
  toolStart?: ExecutionEvent;
  toolResult?: ExecutionEvent;
}

function reconstructSteps(events: ExecutionEvent[]): {
  steps: SubagentStep[];
  thinkingTotal: number;
} {
  const steps: SubagentStep[] = [];
  let thinkingTotal = 0;
  const currentThinking: string[] = [];
  const pendingTools: ExecutionEvent[] = [];

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
    if (e.event === 'thinking_delta') {
      if (e.content) currentThinking.push(e.content);
    } else if (e.event === 'usage') {
      // thinking_ended maps to `usage`; flush the accumulated thinking.
      flushThinking();
    } else if (e.event === 'tool_started') {
      flushThinking();
      pendingTools.push(e);
    } else if (e.event === 'tool_completed') {
      const name = String(e.name ?? '');
      const idx = pendingTools.findIndex((p) => String(p.name ?? '') === name);
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

function subagentResult(run: SubagentRunState): string {
  // SubagentRunState carries the final output directly (no need to dig it out
  // of a `worker_completed` event payload like the legacy store did).
  if (run.output) return run.output;
  return run.events
    .filter((e) => e.event === 'token_delta')
    .map((e) => String(e.content ?? ''))
    .join('')
    .trim();
}

export const SubagentStreamBlock = memo(function SubagentStreamBlock({
  run,
  allRuns,
}: SubagentStreamBlockProps) {
  const [expanded, setExpanded] = useState(run.status === 'running');
  const selectSubagent = useSubagentDetailStore((state) => state.selectSubagent);
  const [sectionExpanded, setSectionExpanded] = useState({
    prompt: false,
    process: true,
    result: true,
  });

  const progress = useMemo(() => computeSubagentProgress(run), [run.events, run.status]);
  const summary = progressSummary(progress);
  const { steps } = useMemo(() => reconstructSteps(run.events), [run.events]);
  const result = useMemo(() => subagentResult(run), [run.events, run.output]);

  const children = useMemo(
    () => allRuns.filter((w) => w.parent === run.subagentRunId),
    [allRuns, run.subagentRunId]
  );

  const statusIcon =
    run.status === 'running' ? (
      <Loader2 size={11} className="animate-spin" style={{ color: 'var(--color-info)' }} />
    ) : run.status === 'completed' ? (
      <CheckCircle2 size={11} style={{ color: 'var(--color-success)' }} />
    ) : run.status === 'failed' ? (
      <AlertCircle size={11} style={{ color: 'var(--color-error)' }} />
    ) : (
      <Circle size={11} style={{ color: 'var(--text-tertiary)' }} />
    );

  return (
    <div className="my-0.5 rounded-sm px-2 py-1 hover:bg-[var(--bg-hover)] transition-colors">
      {/* Header (always visible): title + status + progress summary */}
      <div className="flex w-full items-center gap-1.5 text-[11px]">
        <button
          type="button"
          onClick={() => setExpanded((e) => !e)}
          className="shrink-0 text-[var(--text-tertiary)] transition-colors hover:text-[var(--text-primary)]"
          aria-label={expanded ? '折叠 subagent' : '展开 subagent'}
        >
          {expanded ? <ChevronDown size={10} /> : <ChevronRight size={10} />}
        </button>
        <button
          type="button"
          onClick={() => selectSubagent(run.subagentRunId)}
          className="flex min-w-0 flex-1 items-center gap-1.5 text-left"
        >
          {statusIcon}
          <span className="truncate font-medium text-[var(--text-primary)]">
            {run.agent || run.subagentRunId}
          </span>
          <span className="ml-auto shrink-0 text-[10px] text-[var(--text-tertiary)]">
            {statusLabel(progress.status)}
            {summary ? ` · ${summary}` : ''}
          </span>
        </button>
      </div>

      {expanded && (
        <div className="mt-1.5 space-y-1.5 pl-1">
          {/* Prompt */}
          {run.task && (
            <div>
              <button
                onClick={() => setSectionExpanded((s) => ({ ...s, prompt: !s.prompt }))}
                className="flex items-center gap-1 text-[10px] font-medium text-[var(--text-tertiary)]"
              >
                {sectionExpanded.prompt ? <ChevronDown size={9} /> : <ChevronRight size={9} />}
                提示词
              </button>
              {sectionExpanded.prompt && <MarkdownContent content={run.task} className="text-sm" />}
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
                    // item (thinking/tool/subagent) is a peer in the flow.
                    return (
                      <div key={i}>
                        <div className="mb-0.5 flex items-center gap-1">
                          <Brain size={9} className="text-[var(--color-purple)]" />
                          <span className="text-[9px] font-medium text-[var(--color-purple)]">
                            思考
                          </span>
                        </div>
                        <MarkdownContent content={step.content || ''} className="text-sm" />
                      </div>
                    );
                  }
                  const name = String(step.toolStart?.name ?? 'tool');
                  const args = step.toolStart?.args ?? {};
                  const resultStr = String(step.toolResult?.result ?? '');
                  const success = step.toolResult?.success !== false;
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
                      <SubagentStreamBlock
                        key={child.subagentRunId}
                        run={child}
                        allRuns={allRuns}
                      />
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
                <MarkdownContent content={result} className="text-sm" maxHeight={400} />
              )}
            </div>
          )}
        </div>
      )}
    </div>
  );
});
