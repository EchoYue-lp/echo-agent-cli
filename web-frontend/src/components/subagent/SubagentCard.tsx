import { useState, useEffect } from 'react';
import { Bot, CheckCircle, XCircle, X, Loader2, ChevronDown, ChevronRight } from 'lucide-react';
import type { SubagentState } from '../../stores/subagentStore';

interface Props {
  subagent: SubagentState;
}

function formatDuration(ms: number): string {
  if (ms < 1000) return `${ms}ms`;
  const s = ms / 1000;
  if (s < 60) return `${s.toFixed(1)}s`;
  const m = Math.floor(s / 60);
  const sec = s % 60;
  return `${m}m ${sec.toFixed(0)}s`;
}

function ElapsedTimer({ startedAt }: { startedAt: number }) {
  const [elapsed, setElapsed] = useState(Date.now() - startedAt);
  useEffect(() => {
    const t = setInterval(() => setElapsed(Date.now() - startedAt), 1000);
    return () => clearInterval(t);
  }, [startedAt]);
  return <span className="text-[10px] tabular-nums text-[var(--text-tertiary)]">{formatDuration(elapsed)}</span>;
}

export function SubagentCard({ subagent: s }: Props) {
  const [expanded, setExpanded] = useState(false);

  const Icon =
    s.status === 'running' ? (
      <Loader2 size={14} className="animate-spin text-blue-400" />
    ) : s.status === 'completed' ? (
      <CheckCircle size={14} className="text-green-400" />
    ) : s.status === 'failed' ? (
      <XCircle size={14} className="text-red-400" />
    ) : (
      <X size={14} className="text-gray-400" />
    );

  return (
    <div className="rounded-lg border border-[var(--border-primary)] bg-[var(--bg-secondary)] px-3 py-2 text-xs">
      <button
        className="flex w-full items-center gap-2 text-left"
        onClick={() => setExpanded(!expanded)}
      >
        <Bot size={14} className="text-[var(--text-secondary)] shrink-0" />
        <span className="font-medium text-[var(--text-primary)] truncate flex-1">{s.agent}</span>
        {s.tokensUsed != null && (
          <span className="text-[10px] tabular-nums text-[var(--text-tertiary)]">{s.tokensUsed} tok</span>
        )}
        {Icon}
        {s.status === 'running' ? (
          <ElapsedTimer startedAt={s.startedAt} />
        ) : s.durationMs != null ? (
          <span className="text-[10px] tabular-nums text-[var(--text-tertiary)]">
            {formatDuration(s.durationMs)}
          </span>
        ) : null}
      </button>
      {expanded && (
        <div className="mt-2 space-y-1 border-t border-[var(--border-primary)] pt-2 text-[var(--text-tertiary)]">
          {s.task && (
            <div>
              <span className="font-medium">Task:</span>{' '}
              <span className="break-all">{s.task.slice(0, 200)}</span>
            </div>
          )}
          <div>
            <span className="font-medium">Mode:</span> {s.mode}
          </div>
          <div>
            <span className="font-medium">Status:</span>{' '}
            <span
              className={
                s.status === 'completed'
                  ? 'text-green-400'
                  : s.status === 'failed'
                  ? 'text-red-400'
                  : s.status === 'running'
                  ? 'text-blue-400'
                  : ''
              }
            >
              {s.status}
            </span>
          </div>
          {s.iterationCount != null && s.iterationCount > 0 && (
            <div>
              <span className="font-medium">Iterations:</span> {s.iterationCount}
            </div>
          )}
          {s.error && (
            <div className="text-red-400 break-all">
              <span className="font-medium">Error:</span> {s.error}
            </div>
          )}
        </div>
      )}
    </div>
  );
}

export function SubagentPanel({ subagents }: { subagents: Record<string, SubagentState> }) {
  const entries = Object.values(subagents);
  const [showDone, setShowDone] = useState(true);
  if (entries.length === 0) return null;

  const running = entries.filter((s) => s.status === 'running');
  const completed = entries.filter((s) => s.status === 'completed');
  const failed = entries.filter((s) => s.status === 'failed');
  const done = completed.length + failed.length;
  const total = entries.length;

  return (
    <div className="space-y-2">
      {/* Aggregate progress bar (Phase 5.4) */}
      <div className="flex items-center gap-2 px-1">
        <span className="text-xs font-medium text-[var(--text-secondary)]">
          Subagents
        </span>
        {running.length > 0 && (
          <span className="text-[10px] text-blue-400 animate-pulse">{running.length} active</span>
        )}
        <span className="text-[10px] text-[var(--text-tertiary)] flex-1 text-right">
          {done}/{total} done
        </span>
      </div>
      {total > 1 && (
        <div className="h-1 w-full rounded-full bg-[var(--border-primary)] overflow-hidden">
          <div
            className="h-full rounded-full bg-green-500 transition-all duration-500"
            style={{ width: `${total > 0 ? (done / total) * 100 : 0}%` }}
          />
        </div>
      )}

      {/* Running subagents (always visible) */}
      {running.map((s) => (
        <SubagentCard key={`${s.parent}:${s.agent}`} subagent={s} />
      ))}

      {/* Completed/failed subagents (collapsible) */}
      {done > 0 && (
        <>
          <button
            className="flex items-center gap-1 text-[10px] text-[var(--text-tertiary)] hover:text-[var(--text-secondary)] px-1"
            onClick={() => setShowDone(!showDone)}
          >
            {showDone ? <ChevronDown size={12} /> : <ChevronRight size={12} />}
            {done} completed/failed
          </button>
          {showDone && (
            <div className="space-y-1 opacity-60">
              {completed.map((s) => (
                <SubagentCard key={`${s.parent}:${s.agent}`} subagent={s} />
              ))}
              {failed.map((s) => (
                <SubagentCard key={`${s.parent}:${s.agent}`} subagent={s} />
              ))}
            </div>
          )}
        </>
      )}
    </div>
  );
}
