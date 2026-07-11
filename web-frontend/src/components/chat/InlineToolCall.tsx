import { memo, useEffect, useMemo, useState } from 'react';
import {
  Check,
  ChevronDown,
  ChevronRight,
  CircleStop,
  Copy,
  LoaderCircle,
  Terminal,
  Wrench,
  X,
} from 'lucide-react';
import type { ToolExecution } from '../../types/api';

interface InlineToolCallProps {
  toolCall: ToolExecution;
  index: number;
}

function commandFor(tool: ToolExecution): string {
  if (tool.name === 'shell' && tool.args && typeof tool.args === 'object') {
    const command = (tool.args as Record<string, unknown>).command;
    if (typeof command === 'string') return command;
  }
  const args = tool.args == null ? '' : JSON.stringify(tool.args);
  return args ? `${tool.name} ${args}` : tool.name;
}

function tail(text: string, count = 6): string {
  const lines = text.split('\n');
  return lines.slice(Math.max(0, lines.length - count)).join('\n').trimEnd();
}

export const InlineToolCall = memo(function InlineToolCall({
  toolCall,
  index: _index,
}: InlineToolCallProps) {
  const [expanded, setExpanded] = useState(false);
  const [copied, setCopied] = useState<string | null>(null);
  const [now, setNow] = useState(Date.now());

  useEffect(() => {
    if (toolCall.status !== 'running') return;
    const timer = window.setInterval(() => setNow(Date.now()), 250);
    return () => window.clearInterval(timer);
  }, [toolCall.status]);

  const command = useMemo(() => commandFor(toolCall), [toolCall]);
  const elapsed = Math.max(0, (toolCall.finishedAt ?? now) - toolCall.startedAt) / 1000;
  const durationMs = Number(toolCall.metadata?.duration_ms);
  const duration = Number.isFinite(durationMs) ? durationMs / 1000 : elapsed;
  const exitCode = toolCall.metadata?.exit_code;
  const failed = toolCall.status === 'failed';
  const running = toolCall.status === 'running';
  const preview = tail(
    failed
      ? toolCall.stderr || toolCall.result || toolCall.stdout
      : toolCall.stdout || toolCall.log || toolCall.stderr || toolCall.progress?.message || ''
  );
  const fullOutput = [
    toolCall.stdout && `stdout\n${toolCall.stdout}`,
    toolCall.stderr && `stderr\n${toolCall.stderr}`,
    toolCall.log && `log\n${toolCall.log}`,
  ]
    .filter(Boolean)
    .join('\n\n');

  const copyText = async (text: string, label: string) => {
    await navigator.clipboard.writeText(text);
    setCopied(label);
    window.setTimeout(() => setCopied(null), 1500);
  };

  const statusIcon = running ? (
    <LoaderCircle size={12} className="animate-spin text-[var(--accent)]" />
  ) : toolCall.status === 'succeeded' ? (
    <Check size={12} className="text-[var(--color-success)]" />
  ) : failed ? (
    <X size={12} className="text-[var(--color-error)]" />
  ) : (
    <CircleStop size={12} className="text-[var(--text-tertiary)]" />
  );

  return (
    <div className="my-1 min-w-0 pl-2 font-mono text-[11px]">
      <div className="flex min-h-6 min-w-0 items-start gap-1.5 py-0.5">
        <button
          type="button"
          onClick={() => setExpanded((value) => !value)}
          className="mt-0.5 shrink-0 text-[var(--text-tertiary)]"
          title={expanded ? '折叠工具输出' : '展开工具输出'}
        >
          {expanded ? <ChevronDown size={11} /> : <ChevronRight size={11} />}
        </button>
        <span className="mt-0.5 shrink-0">{statusIcon}</span>
        {toolCall.name === 'shell' ? (
          <Terminal size={12} className="mt-0.5 shrink-0 text-[var(--text-tertiary)]" />
        ) : (
          <Wrench size={12} className="mt-0.5 shrink-0 text-[var(--text-tertiary)]" />
        )}
        <button
          type="button"
          onClick={() => setExpanded((value) => !value)}
          className="min-w-0 flex-1 break-words text-left leading-5 text-[var(--text-primary)]"
        >
          {command}
        </button>
        <span className="shrink-0 pt-0.5 tabular-nums text-[var(--text-tertiary)]">
          {duration.toFixed(1)}s{exitCode == null ? '' : ` · exit ${exitCode}`}
        </span>
        <button
          type="button"
          onClick={() => copyText(command, 'command')}
          className="mt-0.5 shrink-0 text-[var(--text-tertiary)] hover:text-[var(--text-primary)]"
          title="复制命令"
        >
          {copied === 'command' ? <Check size={11} /> : <Copy size={11} />}
        </button>
      </div>

      {!expanded && preview && (
        <pre
          className={`ml-[46px] max-h-[7.5rem] overflow-hidden whitespace-pre-wrap break-words text-[10px] leading-[1.25rem] ${failed ? 'text-[var(--color-error)]' : 'text-[var(--text-tertiary)]'}`}
        >
          {preview}
        </pre>
      )}

      {expanded && (
        <div className="ml-[46px] mt-1 min-w-0 border-l border-[var(--border-primary)] pl-2">
          <div className="mb-1 flex items-center justify-between text-[9px] uppercase text-[var(--text-tertiary)]">
            <span>{toolCall.truncated ? '输出 · 已截断' : '输出'}</span>
            {fullOutput && (
              <button
                type="button"
                onClick={() => copyText(fullOutput, 'output')}
                className="flex items-center gap-1 normal-case hover:text-[var(--text-primary)]"
              >
                {copied === 'output' ? <Check size={10} /> : <Copy size={10} />}
                {copied === 'output' ? '已复制' : '复制'}
              </button>
            )}
          </div>
          <pre className="max-h-64 overflow-auto whitespace-pre-wrap break-words bg-[var(--bg-code)] p-2 text-[10px] leading-relaxed text-[var(--color-code-text)]">
            {fullOutput || toolCall.progress?.message || '暂无输出'}
          </pre>
        </div>
      )}
    </div>
  );
});
