import { useState, memo } from 'react';
import { ChevronDown, ChevronRight, Wrench, Copy, Check } from 'lucide-react';

// We use a lightweight inline type to avoid importing generated types that may not have the exact shape.
interface ToolCallInfo {
  name: string;
  args?: unknown;
  result?: string;
  success?: boolean;
}

interface InlineToolCallProps {
  toolCall: ToolCallInfo;
  /** 1-based index among tool calls in this round */
  index: number;
}

/**
 * One tool call rendered as a lightweight inline collapsible row in the
 * one-stream layout: a one-line event that can expand for args/result.
 */
export const InlineToolCall = memo(function InlineToolCall({
  toolCall,
  index: _index,
}: InlineToolCallProps) {
  const [expanded, setExpanded] = useState(false);
  const [copied, setCopied] = useState<string | null>(null);

  const statusColor = toolCall.success !== false ? 'var(--color-success)' : 'var(--color-error)';
  const statusLabel = toolCall.success !== false ? '✓' : '✗';

  // One-line arg preview: first string-ish value, truncated.
  const argPreview = (() => {
    const args = toolCall.args;
    if (args == null) return '';
    if (typeof args === 'string') return args;
    if (typeof args === 'object') {
      const entries = Object.entries(args as Record<string, unknown>);
      if (entries.length === 0) return '';
      const [k, v] = entries[0];
      const vStr = typeof v === 'string' ? v : JSON.stringify(v);
      return `${k}: ${vStr}`;
    }
    return String(args);
  })();
  const argPreviewTruncated = argPreview.length > 60 ? argPreview.slice(0, 60) + '…' : argPreview;

  const copyText = (text: string, label: string) => {
    navigator.clipboard.writeText(text);
    setCopied(label);
    setTimeout(() => setCopied(null), 2000);
  };

  return (
    <div className="my-0.5 pl-3">
      <button
        onClick={() => setExpanded((e) => !e)}
        className="flex w-full items-center gap-1.5 py-0.5 text-left text-[11px]"
      >
        {expanded ? (
          <ChevronDown size={10} className="shrink-0 text-[var(--text-tertiary)]" />
        ) : (
          <ChevronRight size={10} className="shrink-0 text-[var(--text-tertiary)]" />
        )}
        <Wrench size={10} className="shrink-0" style={{ color: statusColor }} />
        <span className="font-medium text-[var(--text-primary)]">{toolCall.name}</span>
        {argPreviewTruncated && (
          <span className="truncate text-[var(--text-tertiary)]">{argPreviewTruncated}</span>
        )}
        <span className="ml-auto shrink-0" style={{ color: statusColor }}>
          {statusLabel}
        </span>
      </button>
      {expanded && (
        <div className="mt-1 space-y-2 pb-1">
          <div>
            <div className="mb-0.5 flex items-center justify-between">
              <span className="text-[9px] font-medium uppercase text-[var(--text-tertiary)]">
                参数
              </span>
              <button
                onClick={(e) => {
                  e.stopPropagation();
                  copyText(JSON.stringify(toolCall.args ?? {}, null, 2), 'args');
                }}
                className="flex items-center gap-0.5 text-[10px] text-[var(--text-tertiary)]"
              >
                {copied === 'args' ? <Check size={9} /> : <Copy size={9} />}
                {copied === 'args' ? '已复制' : '复制'}
              </button>
            </div>
            <pre className="max-h-32 overflow-auto rounded-md bg-[var(--bg-code)] p-2 text-[10px] leading-relaxed text-[var(--color-code-text)]">
              {JSON.stringify(toolCall.args ?? {}, null, 2)}
            </pre>
          </div>
          {toolCall.result !== undefined && (
            <div>
              <div className="mb-0.5 flex items-center justify-between">
                <span className="text-[9px] font-medium uppercase text-[var(--text-tertiary)]">
                  结果
                </span>
                <button
                  onClick={(e) => {
                    e.stopPropagation();
                    copyText(toolCall.result ?? '', 'result');
                  }}
                  className="flex items-center gap-0.5 text-[10px] text-[var(--text-tertiary)]"
                >
                  {copied === 'result' ? <Check size={9} /> : <Copy size={9} />}
                  {copied === 'result' ? '已复制' : '复制'}
                </button>
              </div>
              <pre className="max-h-40 overflow-auto rounded-md bg-[var(--bg-code)] p-2 text-[10px] leading-relaxed text-[var(--color-code-text)]">
                {(toolCall.result ?? '').length > 2000
                  ? (toolCall.result ?? '').slice(0, 2000) + '\n...'
                  : (toolCall.result ?? '')}
              </pre>
            </div>
          )}
        </div>
      )}
    </div>
  );
});
