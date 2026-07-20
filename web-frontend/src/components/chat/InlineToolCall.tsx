import { memo, useEffect, useMemo, useState } from 'react';
import {
  AlertTriangle,
  Check,
  ChevronDown,
  ChevronRight,
  CircleStop,
  Copy,
  ExternalLink,
  FileSearch,
  FileText,
  Globe,
  LoaderCircle,
  Pencil,
  Plug,
  Terminal,
  Workflow,
  Wrench,
  X,
} from 'lucide-react';
import type { ToolExecution } from '../../types/api';
import { errorMessage, fileSystem } from '../../lib/tauri-bridge';
import { describeToolExecution } from './tools/toolRenderers';

interface InlineToolCallProps {
  toolCall: ToolExecution;
  index: number;
}

function formatToolArgs(args: unknown): string {
  if (args == null) return '';
  try {
    return JSON.stringify(args, null, 2) ?? String(args);
  } catch {
    return String(args);
  }
}

export const InlineToolCall = memo(function InlineToolCall({
  toolCall,
  index: _index,
}: InlineToolCallProps) {
  const [expanded, setExpanded] = useState(false);
  const [copied, setCopied] = useState<string | null>(null);
  const [artifactError, setArtifactError] = useState<string | null>(null);
  const [now, setNow] = useState(Date.now());

  useEffect(() => {
    if (toolCall.status !== 'running') return;
    const timer = window.setInterval(() => setNow(Date.now()), 250);
    return () => window.clearInterval(timer);
  }, [toolCall.status]);

  const descriptor = useMemo(() => describeToolExecution(toolCall), [toolCall]);
  const elapsed = Math.max(0, (toolCall.finishedAt ?? now) - toolCall.startedAt) / 1000;
  const durationMs = Number(toolCall.metadata?.duration_ms);
  const duration = Number.isFinite(durationMs) ? durationMs / 1000 : elapsed;
  const exitCode = toolCall.metadata?.exit_code;
  const failure = toolCall.failure;
  const artifactPath = toolCall.metadata?.artifact_path;
  const artifactBytes = Number(toolCall.metadata?.artifact_bytes);
  const artifactSize = Number.isFinite(artifactBytes)
    ? artifactBytes >= 1024 * 1024
      ? `${(artifactBytes / (1024 * 1024)).toFixed(1)} MiB`
      : `${Math.max(0, Math.round(artifactBytes / 1024))} KiB`
    : null;
  const artifactHash = toolCall.metadata?.artifact_sha256?.slice(0, 12);
  const failed = toolCall.status === 'failed';
  const running = toolCall.status === 'running';
  const argsText = useMemo(() => formatToolArgs(toolCall.args), [toolCall.args]);
  const summaryText = descriptor.detail
    ? `${descriptor.title} ${descriptor.detail}`
    : descriptor.title;
  const fullOutput = [
    toolCall.stdout && `stdout\n${toolCall.stdout}`,
    toolCall.stderr && `stderr\n${toolCall.stderr}`,
    toolCall.log && `log\n${toolCall.log}`,
  ]
    .filter(Boolean)
    .join('\n\n');
  const outputSections = [
    toolCall.stdout && {
      label: 'stdout',
      text: toolCall.stdout,
      tone: 'text-[var(--color-code-text)]',
    },
    toolCall.stderr && {
      label: 'stderr',
      text: toolCall.stderr,
      tone: 'text-[var(--color-error)]',
    },
    toolCall.log && { label: 'log', text: toolCall.log, tone: 'text-[var(--text-secondary)]' },
  ].filter((section): section is { label: string; text: string; tone: string } => Boolean(section));

  const copyText = async (text: string, label: string) => {
    await navigator.clipboard.writeText(text);
    setCopied(label);
    window.setTimeout(() => setCopied(null), 1500);
  };

  const openArtifact = async () => {
    if (!artifactPath) return;
    setArtifactError(null);
    try {
      await fileSystem.openArtifact(artifactPath);
    } catch (error) {
      const message = errorMessage(error);
      setArtifactError(message.includes('not found') ? '完整日志 artifact 已缺失' : message);
    }
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
  const toolIcon =
    descriptor.kind === 'shell' ? (
      <Terminal size={12} className="mt-0.5 shrink-0 text-[var(--text-tertiary)]" />
    ) : descriptor.kind === 'read' ? (
      <FileText size={12} className="mt-0.5 shrink-0 text-[var(--text-tertiary)]" />
    ) : descriptor.kind === 'write' ? (
      <Pencil size={12} className="mt-0.5 shrink-0 text-[var(--text-tertiary)]" />
    ) : descriptor.kind === 'search' ? (
      <FileSearch size={12} className="mt-0.5 shrink-0 text-[var(--text-tertiary)]" />
    ) : descriptor.kind === 'browser' ? (
      <Globe size={12} className="mt-0.5 shrink-0 text-[var(--text-tertiary)]" />
    ) : descriptor.kind === 'mcp' ? (
      <Plug size={12} className="mt-0.5 shrink-0 text-[var(--text-tertiary)]" />
    ) : descriptor.kind === 'task' ? (
      <Workflow size={12} className="mt-0.5 shrink-0 text-[var(--text-tertiary)]" />
    ) : (
      <Wrench size={12} className="mt-0.5 shrink-0 text-[var(--text-tertiary)]" />
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
        {toolIcon}
        <button
          type="button"
          onClick={() => setExpanded((value) => !value)}
          className="min-w-0 flex-1 overflow-hidden whitespace-nowrap text-left leading-5 text-[var(--text-primary)]"
          title={summaryText}
        >
          <span className="block truncate">
            <span>{descriptor.title}</span>
            {descriptor.detail && (
              <span className="ml-1.5 text-[var(--text-tertiary)]">· {descriptor.detail}</span>
            )}
          </span>
        </button>
        <span className="shrink-0 pt-0.5 tabular-nums text-[var(--text-tertiary)]">
          {failure ? `${failure.category} · ` : ''}
          {duration.toFixed(1)}s{exitCode == null ? '' : ` · exit ${exitCode}`}
        </span>
        <button
          type="button"
          onClick={() => copyText(descriptor.title, 'command')}
          className="mt-0.5 shrink-0 text-[var(--text-tertiary)] hover:text-[var(--text-primary)]"
          title={descriptor.kind === 'shell' ? '复制命令' : '复制摘要'}
        >
          {copied === 'command' ? <Check size={11} /> : <Copy size={11} />}
        </button>
        {artifactPath && (
          <button
            type="button"
            onClick={openArtifact}
            className="mt-0.5 shrink-0 text-[var(--accent)] hover:text-[var(--text-primary)]"
            title="打开完整日志 artifact"
          >
            <ExternalLink size={11} />
          </button>
        )}
      </div>

      {expanded && (
        <div className="ml-[46px] mt-1 min-w-0 border-l border-[var(--border-primary)] pl-2">
          {argsText && (
            <div className="mb-2">
              <div className="mb-1 flex items-center justify-between text-[9px] uppercase text-[var(--text-tertiary)]">
                <span>参数</span>
                <button
                  type="button"
                  onClick={() => copyText(argsText, 'args')}
                  className="flex items-center gap-1 normal-case hover:text-[var(--text-primary)]"
                >
                  {copied === 'args' ? <Check size={10} /> : <Copy size={10} />}
                  {copied === 'args' ? '已复制' : '复制'}
                </button>
              </div>
              <pre className="max-h-40 overflow-auto whitespace-pre-wrap break-words bg-[var(--bg-code)] p-2 text-[10px] leading-relaxed text-[var(--color-code-text)]">
                {argsText}
              </pre>
            </div>
          )}
          <div className="mb-1 flex items-center justify-between text-[9px] uppercase text-[var(--text-tertiary)]">
            <span>{toolCall.truncated ? '输出 · 已截断' : '输出'}</span>
            {fullOutput && (
              <button
                type="button"
                onClick={() => copyText(fullOutput, 'output')}
                className="flex items-center gap-1 normal-case hover:text-[var(--text-primary)]"
              >
                {copied === 'output' ? <Check size={10} /> : <Copy size={10} />}
                {copied === 'output' ? '已复制' : '复制全部'}
              </button>
            )}
          </div>
          {failure && (
            <div className="mb-2 border-l-2 border-[var(--color-error)] pl-2 text-[10px] leading-relaxed text-[var(--text-secondary)]">
              <div>{failure.recovery}</div>
              {failure.postcondition && <div>{failure.postcondition}</div>}
            </div>
          )}
          {artifactPath && (
            <div className="mb-2 flex min-w-0 items-center gap-2 border-l-2 border-[var(--accent)] pl-2 text-[10px] text-[var(--text-secondary)]">
              {artifactError ? (
                <AlertTriangle size={11} className="shrink-0 text-[var(--color-warning)]" />
              ) : (
                <FileText size={11} className="shrink-0 text-[var(--accent)]" />
              )}
              <span className="min-w-0 flex-1 truncate" title={artifactPath}>
                {artifactError || `完整日志${artifactSize ? ` · ${artifactSize}` : ''}`}
                {artifactHash ? ` · sha256 ${artifactHash}` : ''}
              </span>
              <button
                type="button"
                onClick={openArtifact}
                className="flex shrink-0 items-center gap-1 text-[var(--accent)] hover:text-[var(--text-primary)]"
                title="打开完整日志 artifact"
              >
                <ExternalLink size={10} />
                打开
              </button>
            </div>
          )}
          {outputSections.length > 0 ? (
            <div className="max-h-64 space-y-2 overflow-auto bg-[var(--bg-code)] p-2">
              {outputSections.map((section) => (
                <section key={section.label}>
                  <div className="mb-0.5 flex items-center justify-between text-[9px] text-[var(--text-tertiary)]">
                    <span>{section.label}</span>
                    <button
                      type="button"
                      onClick={() => copyText(section.text, section.label)}
                      className="flex items-center gap-1 hover:text-[var(--text-primary)]"
                    >
                      {copied === section.label ? <Check size={9} /> : <Copy size={9} />}
                      {copied === section.label ? '已复制' : '复制'}
                    </button>
                  </div>
                  <pre
                    className={`whitespace-pre-wrap break-words text-[10px] leading-relaxed ${section.tone}`}
                  >
                    {section.text}
                  </pre>
                </section>
              ))}
            </div>
          ) : (
            <pre className="max-h-64 overflow-auto whitespace-pre-wrap break-words bg-[var(--bg-code)] p-2 text-[10px] leading-relaxed text-[var(--color-code-text)]">
              {toolCall.progress?.message || '暂无输出'}
            </pre>
          )}
        </div>
      )}
    </div>
  );
});
