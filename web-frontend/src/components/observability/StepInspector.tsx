import type { TraceEvent, TraceKind } from '../../api/endpoints';

interface StepInspectorProps {
  event: TraceEvent | null;
}

function kindDetail(kind: TraceKind): { title: string; fields: [string, string][] } {
  switch (kind.type) {
    case 'llm_call':
      return {
        title: `LLM Call — ${kind.model}`,
        fields: [
          ['Model', kind.model],
          ['Input Tokens', kind.input_tokens.toLocaleString()],
          ['Output Tokens', kind.output_tokens.toLocaleString()],
          ['Total Tokens', (kind.input_tokens + kind.output_tokens).toLocaleString()],
        ],
      };
    case 'tool_call':
      return {
        title: `Tool Call — ${kind.tool}`,
        fields: [
          ['Tool', kind.tool],
          ['Success', kind.success ? 'Yes' : 'No'],
          ...(kind.error ? [['Error', kind.error] as [string, string]] : []),
        ],
      };
    case 'agent_step':
      return {
        title: `Agent Step #${kind.step_number}`,
        fields: [
          ['Step Number', String(kind.step_number)],
          ...(kind.thought_preview ? [['Thought Preview', kind.thought_preview] as [string, string]] : []),
        ],
      };
    case 'pipeline_stage':
      return {
        title: `Pipeline: ${kind.pipeline}`,
        fields: [
          ['Pipeline', kind.pipeline],
          ['Stage', kind.stage],
        ],
      };
    case 'memory_access':
      return {
        title: `Memory: ${kind.operation}`,
        fields: [
          ['Operation', kind.operation],
          ...(kind.results_count != null ? [['Results Count', String(kind.results_count)] as [string, string]] : []),
        ],
      };
    case 'mcp_call':
      return {
        title: `MCP: ${kind.server}`,
        fields: [
          ['Server', kind.server],
          ['Method', kind.method],
        ],
      };
    case 'context_compression':
      return {
        title: 'Context Compression',
        fields: [
          ['Before Messages', String(kind.before_messages)],
          ['After Messages', String(kind.after_messages)],
          ['Before Tokens', kind.before_tokens.toLocaleString()],
          ['After Tokens', kind.after_tokens.toLocaleString()],
          ['Reduction', `${(((kind.before_tokens - kind.after_tokens) / Math.max(kind.before_tokens, 1)) * 100).toFixed(1)}%`],
        ],
      };
  }
}

export function StepInspector({ event }: StepInspectorProps) {
  const s = {
    text: 'var(--text-primary)',
    textSec: 'var(--text-secondary)',
    textTer: 'var(--text-tertiary)',
    border: 'var(--border-primary)',
    bg: 'var(--bg-primary)',
    bgHover: 'var(--bg-hover)',
  };

  if (!event) {
    return (
      <div className="py-8 text-center text-xs" style={{ color: s.textTer }}>
        Select an event to inspect its details
      </div>
    );
  }

  const { title, fields } = kindDetail(event.kind);
  const hasMetadata = event.metadata && Object.keys(event.metadata).length > 0;

  return (
    <div className="space-y-3">
      {/* Title */}
      <h4 className="text-sm font-semibold" style={{ color: s.text }}>{title}</h4>

      {/* Timestamp & duration */}
      <div className="flex items-center gap-4 text-[10px] font-mono" style={{ color: s.textTer }}>
        <span>{new Date(event.timestamp).toLocaleString()}</span>
        {event.duration_ms != null && (
          <span>{event.duration_ms < 1000 ? `${event.duration_ms}ms` : `${(event.duration_ms / 1000).toFixed(2)}s`}</span>
        )}
      </div>

      {/* Fields table */}
      <div className="rounded-lg border overflow-hidden" style={{ borderColor: s.border }}>
        <table className="w-full text-xs">
          <tbody>
            {fields.map(([label, value]) => (
              <tr key={label} className="border-b last:border-b-0" style={{ borderColor: s.border }}>
                <td className="px-3 py-1.5 font-medium whitespace-nowrap" style={{ color: s.textSec, background: s.bgHover }}>
                  {label}
                </td>
                <td className="px-3 py-1.5 break-all" style={{ color: s.text, background: s.bg }}>
                  {value}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>

      {/* Raw metadata */}
      {hasMetadata && (
        <div>
          <h5 className="text-xs font-medium mb-1" style={{ color: s.textSec }}>Metadata</h5>
          <pre
            className="rounded-lg border p-3 text-[10px] font-mono overflow-x-auto whitespace-pre-wrap break-all"
            style={{ borderColor: s.border, background: s.bgHover, color: s.textSec }}
          >
            {JSON.stringify(event.metadata, null, 2)}
          </pre>
        </div>
      )}
    </div>
  );
}
