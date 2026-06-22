import { Activity, CheckCircle, ChevronDown, ChevronRight, Cpu, Route, Wrench, XCircle } from 'lucide-react';
import { useState } from 'react';
import { useConversationRuntimeStore, type ConversationRuntimeEventData } from '../../stores/conversationRuntimeStore';
import MarkdownContent from '../common/MarkdownContent';

function formatRate(value: number | undefined): string {
  if (value == null) return '—';
  return `${(value * 100).toFixed(1)}%`;
}

function formatTokens(n: number | undefined): string {
  if (n == null) return '0';
  return n.toLocaleString();
}

export function ConversationTimeline() {
  const events = useConversationRuntimeStore((s) => s.events);
  const workers = useConversationRuntimeStore((s) => s.workers);

  if (events.length === 0) return null;

  return (
    <div className="conversation-timeline space-y-3 px-1 py-2">
      {events.map((event, i) => (
        <TimelineCard key={i} event={event} workers={workers} />
      ))}
    </div>
  );
}

function TimelineCard({
  event,
  workers,
}: {
  event: ConversationRuntimeEventData;
  workers: Map<string, { workerId: string; title: string; agentRole: string; status: string; summary?: string; filesChanged: string[] }>;
}) {
  const [expanded, setExpanded] = useState(false);

  switch (event.type) {
    case 'route_decision':
      return (
        <div className="rounded-lg border p-3" style={{ borderColor: 'var(--border-secondary)', background: 'var(--bg-secondary)' }}>
          <div className="flex items-center gap-2 text-xs font-medium" style={{ color: 'var(--text-primary)' }}>
            <Route size={14} />
            {event.route === 'normal_chat' ? 'Chat' : event.route === 'complex_runtime' ? 'TaskRuntime' : event.route?.replace(/_/g, ' ')}
            {event.interaction_mode && (
              <span className="rounded px-1.5 py-0.5 text-[10px]" style={{ background: 'var(--bg-hover)', color: 'var(--text-tertiary)' }}>
                {event.interaction_mode}
              </span>
            )}
          </div>
          {event.reason && (
            <div className="mt-1 text-xs" style={{ color: 'var(--text-tertiary)' }}>{event.reason.slice(0, 150)}</div>
          )}
          {event.suggested_workers && event.suggested_workers.length > 0 && (
            <div className="mt-1 flex flex-wrap gap-1">
              {event.suggested_workers.map((w) => (
                <span key={w} className="rounded px-1.5 py-0.5 text-[10px] font-mono" style={{ background: 'var(--bg-hover)', color: 'var(--text-secondary)' }}>
                  {w}
                </span>
              ))}
            </div>
          )}
        </div>
      );

    case 'worker_started': {
      const w = event.worker_id ? workers.get(event.worker_id) : null;
      return (
        <div className="rounded-lg border p-3" style={{ borderColor: 'var(--border-secondary)', background: 'var(--bg-secondary)' }}>
          <button
            onClick={() => setExpanded(!expanded)}
            className="flex w-full items-center gap-2 text-xs font-medium"
            style={{ color: 'var(--text-primary)' }}
          >
            {expanded ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
            <Wrench size={14} />
            {event.title || event.worker_id}
            {w && (
              <span className="ml-auto rounded px-1.5 py-0.5 text-[10px]" style={{
                background: w.status === 'completed' ? 'var(--color-success-bg)' : w.status === 'failed' ? 'var(--color-error-bg)' : 'var(--bg-hover)',
                color: w.status === 'completed' ? 'var(--color-success)' : w.status === 'failed' ? 'var(--color-error)' : 'var(--text-tertiary)',
              }}>
                {w.status === 'completed' ? <CheckCircle size={10} className="inline mr-0.5" /> : w.status === 'failed' ? <XCircle size={10} className="inline mr-0.5" /> : <Activity size={10} className="inline mr-0.5" />}
                {w.status}
              </span>
            )}
          </button>
          {expanded && (
            <div className="mt-2 space-y-1 pl-6 text-xs" style={{ color: 'var(--text-secondary)' }}>
              {event.agent_role && <div>Role: {event.agent_role}</div>}
              {event.task_description && <MarkdownContent content={event.task_description.slice(0, 500)} className="text-[11px]" />}
              {w?.summary && (
                <div className="mt-1 rounded p-2 text-[11px]" style={{ background: 'var(--bg-primary)' }}>
                  <MarkdownContent content={w.summary} />
                </div>
              )}
              {w?.filesChanged && w.filesChanged.length > 0 && (
                <div className="flex flex-wrap gap-1 mt-1">
                  {w.filesChanged.map((f) => (
                    <span key={f} className="rounded px-1 py-0.5 text-[10px] font-mono" style={{ background: 'var(--bg-hover)' }}>{f}</span>
                  ))}
                </div>
              )}
            </div>
          )}
        </div>
      );
    }

    case 'llm_usage':
      return (
        <div className="flex items-center gap-2 rounded-md px-2 py-1 text-[10px]" style={{ color: 'var(--text-tertiary)' }}>
          <Cpu size={10} />
          <span className="font-mono">{event.model || 'unknown'}</span>
          <span>in:{formatTokens(event.input_tokens)} out:{formatTokens(event.output_tokens)}</span>
          <span>cache:{formatRate(event.cached_input_tokens != null && event.input_tokens ? event.cached_input_tokens / event.input_tokens : undefined)}</span>
        </div>
      );

    case 'final_answer':
      return (
        <div className="space-y-2">
          {event.content && (
            <div className="rounded-lg p-3 text-sm" style={{ background: 'var(--bg-secondary)', color: 'var(--text-primary)' }}>
              <MarkdownContent content={event.content} />
            </div>
          )}
          {event.usage_summary != null && (
            <UsageSummaryCard summary={event.usage_summary as Record<string, unknown>} />
          )}
        </div>
      );

    case 'initial_thinking':
      return (
        <div className="flex items-center gap-2 rounded-md px-2 py-1 text-xs" style={{ color: 'var(--text-tertiary)' }}>
          <Activity size={12} className="animate-pulse" />
          {event.worker_id ? `${event.worker_id} thinking...` : 'Thinking...'}
        </div>
      );

    case 'worker_tool_call':
      return (
        <div className="rounded-md px-2 py-1 text-[10px] font-mono" style={{ color: 'var(--text-tertiary)' }}>
          [{event.worker_id}] {event.tool_name}
          {event.success === false && <XCircle size={10} className="inline ml-1" style={{ color: 'var(--color-error)' }} />}
        </div>
      );

    case 'worker_result':
      return (
        <div className="rounded border p-2 text-xs" style={{ borderColor: 'var(--border-secondary)', color: 'var(--text-secondary)' }}>
          <div className="font-medium" style={{ color: 'var(--text-primary)' }}>{event.worker_id}</div>
          {event.summary && <div className="mt-1 text-[11px]"><MarkdownContent content={event.summary.slice(0, 500)} /></div>}
          {event.files_changed && event.files_changed.length > 0 && (
            <div className="mt-1 flex flex-wrap gap-1">
              {event.files_changed.map((f) => (
                <span key={f} className="rounded px-1 py-0.5 text-[10px] font-mono" style={{ background: 'var(--bg-hover)' }}>{f}</span>
              ))}
            </div>
          )}
        </div>
      );

    case 'approval_request':
      return (
        <div className="rounded-lg border p-3" style={{ borderColor: 'var(--color-warning)', background: 'var(--color-warning-bg)' }}>
          <div className="text-xs font-medium" style={{ color: 'var(--text-primary)' }}>
            Approval: {event.tool_name}
          </div>
          {event.prompt && <div className="mt-1 text-xs" style={{ color: 'var(--text-secondary)' }}>{event.prompt.slice(0, 150)}</div>}
        </div>
      );

    case 'error':
      return (
        <div className="rounded-md px-3 py-2 text-xs" style={{ background: 'var(--color-error-bg)', color: 'var(--color-error)' }}>
          [{event.stage}] {event.message}
        </div>
      );

    default:
      return null;
  }
}

function UsageSummaryCard({ summary }: { summary: Record<string, unknown> }) {
  return (
    <div className="rounded-lg border p-3" style={{ borderColor: 'var(--border-secondary)', background: 'var(--bg-secondary)' }}>
      <div className="text-xs font-medium mb-2" style={{ color: 'var(--text-primary)' }}>Usage Summary</div>
      <div className="grid grid-cols-3 gap-2 text-[11px]">
        <div style={{ color: 'var(--text-tertiary)' }}>Input: <span style={{ color: 'var(--text-primary)' }}>{formatTokens(summary.total_input_tokens as number)}</span></div>
        <div style={{ color: 'var(--text-tertiary)' }}>Output: <span style={{ color: 'var(--text-primary)' }}>{formatTokens(summary.total_output_tokens as number)}</span></div>
        <div style={{ color: 'var(--text-tertiary)' }}>Cached: <span style={{ color: 'var(--text-primary)' }}>{formatTokens(summary.total_cached_input_tokens as number)}</span></div>
        <div style={{ color: 'var(--text-tertiary)' }}>LLM calls: <span style={{ color: 'var(--text-primary)' }}>{String(summary.llm_calls || 0)}</span></div>
        <div style={{ color: 'var(--text-tertiary)' }}>Cache rate: <span style={{ color: 'var(--text-primary)' }}>{formatRate(summary.cache_read_rate as number)}</span></div>
      </div>
    </div>
  );
}
