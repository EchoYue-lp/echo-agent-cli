import { useState } from 'react';
import { ChevronDown, ChevronRight, Wrench, Copy, Check } from 'lucide-react';
import type { ToolCallInfo } from '../../types/api';

export function ToolCallCard({ toolCall }: { toolCall: ToolCallInfo }) {
  const [expanded, setExpanded] = useState(false);
  const [copied, setCopied] = useState<string | null>(null);

  const statusColor = toolCall.success ? '#10b981' : '#ef4444';

  const copyText = (text: string, label: string) => {
    navigator.clipboard.writeText(text);
    setCopied(label);
    setTimeout(() => setCopied(null), 2000);
  };

  return (
    <div
      className="my-1 overflow-hidden rounded-lg text-sm"
      style={{
        border: '1px solid var(--border-primary)',
        borderLeft: `3px solid ${statusColor}`,
        background: 'var(--bg-secondary)',
      }}
    >
      <button
        onClick={() => setExpanded(!expanded)}
        className="flex w-full items-center gap-2 px-3 py-2 text-left transition-colors"
      >
        <Wrench size={12} style={{ color: '#f59e0b', flexShrink: 0 }} />
        <span className="truncate font-mono text-xs font-medium" style={{ color: 'var(--text-primary)' }}>
          {toolCall.name}
        </span>
        <span
          className="ml-auto rounded-full px-2 py-0.5 text-[10px] font-medium"
          style={{
            background: toolCall.success ? '#10b98118' : '#ef444418',
            color: statusColor,
          }}
        >
          {toolCall.success ? 'success' : 'failed'}
        </span>
        {expanded
          ? <ChevronDown size={14} style={{ color: 'var(--text-tertiary)', flexShrink: 0 }} />
          : <ChevronRight size={14} style={{ color: 'var(--text-tertiary)', flexShrink: 0 }} />
        }
      </button>

      {expanded && (
        <div className="space-y-2 border-t px-3 pb-3 pt-2" style={{ borderColor: 'var(--border-primary)' }}>
          <div>
            <div className="mb-1 flex items-center justify-between">
              <span className="text-[11px] font-medium uppercase tracking-wider" style={{ color: 'var(--text-tertiary)' }}>
                Arguments
              </span>
              <CopyBtn text={JSON.stringify(toolCall.args, null, 2)} label="args" copied={copied} onCopy={copyText} />
            </div>
            <pre
              className="max-h-40 overflow-auto rounded-lg p-3 text-xs leading-relaxed"
              style={{ background: 'var(--bg-code)', color: '#e2e8f0' }}
            >
              {JSON.stringify(toolCall.args, null, 2)}
            </pre>
          </div>
          <div>
            <div className="mb-1 flex items-center justify-between">
              <span className="text-[11px] font-medium uppercase tracking-wider" style={{ color: 'var(--text-tertiary)' }}>
                Result
              </span>
              <CopyBtn text={toolCall.result} label="result" copied={copied} onCopy={copyText} />
            </div>
            <pre
              className="max-h-40 overflow-auto rounded-lg p-3 text-xs leading-relaxed"
              style={{ background: 'var(--bg-code)', color: '#e2e8f0' }}
            >
              {toolCall.result.length > 2000 ? toolCall.result.slice(0, 2000) + '\n...' : toolCall.result}
            </pre>
          </div>
        </div>
      )}
    </div>
  );
}

function CopyBtn({ text, label, copied, onCopy }: {
  text: string;
  label: string;
  copied: string | null;
  onCopy: (text: string, label: string) => void;
}) {
  return (
    <button
      onClick={(e) => { e.stopPropagation(); onCopy(text, label); }}
      className="flex items-center gap-1 rounded px-1.5 py-0.5 text-[11px] transition-colors"
      style={{ color: 'var(--text-tertiary)' }}
    >
      {copied === label ? <Check size={10} /> : <Copy size={10} />}
      {copied === label ? 'Copied' : 'Copy'}
    </button>
  );
}
