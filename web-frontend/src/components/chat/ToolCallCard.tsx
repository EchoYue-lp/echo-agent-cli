import { useState, useMemo } from 'react';
import { ChevronDown, ChevronRight, Wrench, Copy, Check } from 'lucide-react';
import { StatusBadge } from '../common/StatusBadge';
import type { ToolCallInfo } from '../../generated';
import { ChartCard, extractVegaLiteSpec } from './ChartCard';

export function ToolCallCard({
  toolCall,
  compact = false,
}: {
  toolCall: ToolCallInfo;
  compact?: boolean;
}) {
  const [expanded, setExpanded] = useState(false);
  const [copied, setCopied] = useState<string | null>(null);

  const statusColor = toolCall.success ? 'var(--color-success)' : 'var(--color-error)';

  const chartSpec = useMemo(() => extractVegaLiteSpec(toolCall.result), [toolCall.result]);

  const copyText = (text: string, label: string) => {
    navigator.clipboard.writeText(text);
    setCopied(label);
    setTimeout(() => setCopied(null), 2000);
  };

  return (
    <div
      className={`overflow-hidden rounded-lg border border-[var(--border-primary)] bg-[var(--bg-secondary)] ${compact ? 'text-xs' : 'text-sm'}`}
      style={{ borderLeft: `3px solid ${statusColor}` }}
    >
      <button
        onClick={() => setExpanded(!expanded)}
        className={`flex w-full items-center gap-2 text-left transition-colors ${compact ? 'px-2 py-1' : 'px-3 py-2'}`}
      >
        <Wrench size={compact ? 10 : 12} className="shrink-0 text-amber-500" />
        <span
          className={`truncate font-mono font-medium text-[var(--text-primary)] ${compact ? 'text-[10px]' : 'text-xs'}`}
        >
          {toolCall.name}
        </span>
        <span className="ml-auto">
          <StatusBadge
            status={toolCall.success ? 'success' : 'error'}
            label={toolCall.success ? '成功' : '失败'}
            size="sm"
          />
        </span>
        {expanded ? (
          <ChevronDown size={compact ? 10 : 14} className="shrink-0 text-[var(--text-tertiary)]" />
        ) : (
          <ChevronRight size={compact ? 10 : 14} className="shrink-0 text-[var(--text-tertiary)]" />
        )}
      </button>

      {expanded && (
        <div
          className={`space-y-2 border-t border-[var(--border-primary)] ${compact ? 'px-2 pb-2 pt-1.5' : 'px-3 pb-3 pt-2'}`}
        >
          <div>
            <div className="mb-1 flex items-center justify-between">
              <span
                className={`font-medium uppercase tracking-wider text-[var(--text-tertiary)] ${compact ? 'text-[9px]' : 'text-[11px]'}`}
              >
                参数
              </span>
              <CopyBtn
                text={JSON.stringify(toolCall.args, null, 2)}
                label="args"
                copied={copied}
                onCopy={copyText}
              />
            </div>
            <pre
              className={`overflow-auto rounded-lg bg-[var(--bg-code)] leading-relaxed text-[var(--color-code-text)] ${compact ? 'max-h-32 p-2 text-[10px]' : 'max-h-40 p-3 text-xs'}`}
            >
              {JSON.stringify(toolCall.args, null, 2)}
            </pre>
          </div>
          <div>
            <div className="mb-1 flex items-center justify-between">
              <span
                className={`font-medium uppercase tracking-wider text-[var(--text-tertiary)] ${compact ? 'text-[9px]' : 'text-[11px]'}`}
              >
                结果
              </span>
              <CopyBtn text={toolCall.result} label="result" copied={copied} onCopy={copyText} />
            </div>
            {chartSpec ? (
              <ChartCard spec={chartSpec} />
            ) : (
              <pre
                className={`overflow-auto rounded-lg bg-[var(--bg-code)] leading-relaxed text-[var(--color-code-text)] ${compact ? 'max-h-32 p-2 text-[10px]' : 'max-h-40 p-3 text-xs'}`}
              >
                {toolCall.result.length > 2000
                  ? toolCall.result.slice(0, 2000) + '\n...'
                  : toolCall.result}
              </pre>
            )}
          </div>
        </div>
      )}
    </div>
  );
}

function CopyBtn({
  text,
  label,
  copied,
  onCopy,
}: {
  text: string;
  label: string;
  copied: string | null;
  onCopy: (text: string, label: string) => void;
}) {
  return (
    <button
      onClick={(e) => {
        e.stopPropagation();
        onCopy(text, label);
      }}
      className="flex items-center gap-1 rounded-md px-1.5 py-0.5 text-[11px] text-[var(--text-tertiary)] transition-colors"
    >
      {copied === label ? <Check size={10} /> : <Copy size={10} />}
      {copied === label ? '已复制' : '复制'}
    </button>
  );
}
