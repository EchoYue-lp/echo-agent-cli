import { useMemo } from 'react';
import type { TraceEvent, TraceKind } from '../../api/endpoints';
import {
  Cpu, Wrench, Brain, GitBranch, Database, Server, Archive,
} from 'lucide-react';

interface TraceTimelineProps {
  events: TraceEvent[];
  onSelect?: (event: TraceEvent) => void;
  selectedEvent?: TraceEvent;
}

function kindLabel(kind: TraceKind): string {
  switch (kind.type) {
    case 'llm_call': return `LLM: ${kind.model}`;
    case 'tool_call': return `Tool: ${kind.tool}`;
    case 'agent_step': return `Step #${kind.step_number}`;
    case 'pipeline_stage': return `${kind.pipeline} / ${kind.stage}`;
    case 'memory_access': return `Memory: ${kind.operation}`;
    case 'mcp_call': return `MCP: ${kind.server}.${kind.method}`;
    case 'context_compression': return 'Context Compression';
  }
}

function kindIcon(kind: TraceKind) {
  const size = 13;
  switch (kind.type) {
    case 'llm_call': return <Cpu size={size} />;
    case 'tool_call': return <Wrench size={size} />;
    case 'agent_step': return <Brain size={size} />;
    case 'pipeline_stage': return <GitBranch size={size} />;
    case 'memory_access': return <Database size={size} />;
    case 'mcp_call': return <Server size={size} />;
    case 'context_compression': return <Archive size={size} />;
  }
}

function kindColor(kind: TraceKind): string {
  switch (kind.type) {
    case 'llm_call': return 'var(--color-info, #3b82f6)';
    case 'tool_call':
      return kind.success ? 'var(--color-success, #22c55e)' : 'var(--color-error, #ef4444)';
    case 'agent_step': return 'var(--text-tertiary, #94a3b8)';
    case 'pipeline_stage': return 'var(--color-warning, #f59e0b)';
    case 'memory_access': return '#8b5cf6';
    case 'mcp_call': return '#06b6d4';
    case 'context_compression': return '#ec4899';
  }
}

function formatDuration(ms?: number): string {
  if (ms == null) return '';
  if (ms < 1000) return `${ms}ms`;
  return `${(ms / 1000).toFixed(1)}s`;
}

function formatTime(iso: string): string {
  try {
    const d = new Date(iso);
    return d.toLocaleTimeString(undefined, { hour12: false, hour: '2-digit', minute: '2-digit', second: '2-digit' });
  } catch {
    return iso;
  }
}

export function TraceTimeline({ events, onSelect, selectedEvent }: TraceTimelineProps) {
  const maxDuration = useMemo(() => {
    return Math.max(1, ...events.map((e) => e.duration_ms ?? 0));
  }, [events]);

  const s = {
    text: 'var(--text-primary)',
    textSec: 'var(--text-secondary)',
    textTer: 'var(--text-tertiary)',
    border: 'var(--border-primary)',
    bg: 'var(--bg-primary)',
    bgHover: 'var(--bg-hover)',
  };

  if (events.length === 0) {
    return (
      <div className="py-8 text-center text-xs" style={{ color: s.textTer }}>
        No trace events to display
      </div>
    );
  }

  return (
    <div className="space-y-1">
      {events.map((event, i) => {
        const isSelected = selectedEvent === event;
        const barWidth = event.duration_ms
          ? Math.max(4, (event.duration_ms / maxDuration) * 100)
          : 0;
        const color = kindColor(event.kind);

        return (
          <button
            key={i}
            onClick={() => onSelect?.(event)}
            className="w-full text-left rounded-lg border px-3 py-2 transition-colors"
            style={{
              borderColor: isSelected ? color : s.border,
              background: isSelected ? s.bgHover : s.bg,
            }}
          >
            {/* Header row */}
            <div className="flex items-center gap-2">
              <span style={{ color }}>{kindIcon(event.kind)}</span>
              <span className="text-xs font-medium truncate" style={{ color: s.text }}>
                {kindLabel(event.kind)}
              </span>
              <span className="ml-auto text-[10px] font-mono whitespace-nowrap" style={{ color: s.textTer }}>
                {formatTime(event.timestamp)}
              </span>
            </div>

            {/* Duration bar */}
            {barWidth > 0 && (
              <div className="mt-1.5 flex items-center gap-2">
                <div className="h-1.5 flex-1 rounded-full overflow-hidden" style={{ background: s.bgHover }}>
                  <div
                    className="h-full rounded-full"
                    style={{ width: `${barWidth}%`, background: color, opacity: 0.7 }}
                  />
                </div>
                <span className="text-[10px] font-mono shrink-0" style={{ color: s.textTer }}>
                  {formatDuration(event.duration_ms)}
                </span>
              </div>
            )}

            {/* Subtitle for specific kinds */}
            {event.kind.type === 'llm_call' && (
              <p className="mt-1 text-[10px]" style={{ color: s.textTer }}>
                {event.kind.input_tokens.toLocaleString()} in / {event.kind.output_tokens.toLocaleString()} out tokens
              </p>
            )}
            {event.kind.type === 'tool_call' && event.kind.error && (
              <p className="mt-1 text-[10px]" style={{ color: 'var(--color-error)' }}>
                {event.kind.error}
              </p>
            )}
            {event.kind.type === 'agent_step' && event.kind.thought_preview && (
              <p className="mt-1 text-[10px] truncate" style={{ color: s.textSec }}>
                {event.kind.thought_preview}
              </p>
            )}
          </button>
        );
      })}
    </div>
  );
}
