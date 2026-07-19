import { useState, memo } from 'react';
import type { ChatMessage, ExecutionRound, ToolExecution } from '../../types/api';
import { Bot, Copy, Check, RefreshCw, Pencil, X, ArrowUp, File, Download } from 'lucide-react';
import MarkdownContent from '../common/MarkdownContent';
import { ThinkingSegment } from './ThinkingSegment';
import { InlineToolCall } from './InlineToolCall';
import { ParallelExecutionBlock } from './ParallelExecutionBlock';
import { isSubagentDispatchTool } from './tools/toolRenderers';

interface MessageBubbleProps {
  message: ChatMessage;
  onRegenerate?: () => void;
  onEditAndResend?: (messageId: string, newContent: string) => void;
}

function formatFileSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

async function copyToClipboard(text: string): Promise<boolean> {
  try {
    if (navigator.clipboard?.writeText) {
      await navigator.clipboard.writeText(text);
      return true;
    }
  } catch {}
  const textarea = document.createElement('textarea');
  textarea.value = text;
  textarea.style.position = 'fixed';
  textarea.style.left = '-9999px';
  document.body.appendChild(textarea);
  textarea.select();
  try {
    document.execCommand('copy');
    return true;
  } catch {
    return false;
  } finally {
    document.body.removeChild(textarea);
  }
}

function isImageFile(mime: string): boolean {
  return mime.startsWith('image/');
}

/** Flatten executionRounds (or legacy fields) into an ordered step list. */
interface FlatStep {
  type: 'thinking' | 'tool';
  thinkingContent?: string;
  toolCall?: ToolExecution;
  toolIndex: number;
}

export function flattenSteps(message: ChatMessage): { steps: FlatStep[]; thinkingTotal: number } {
  const steps: FlatStep[] = [];
  let thinkingTotal = 0;

  if (!message.isStreaming && message.executionRounds && message.executionRounds.length > 0) {
    const renderedToolIds = new Set<string>();
    message.executionRounds.forEach((round: ExecutionRound) => {
      if (round.thinking && round.thinking.content.trim()) {
        steps.push({ type: 'thinking', thinkingContent: round.thinking.content, toolIndex: 0 });
        thinkingTotal++;
      }
      round.toolCallIds.forEach((callId) => {
        const tc = message.toolCalls?.find((tool) => tool.id === callId);
        if (tc && !isSubagentDispatchTool(tc.name)) {
          renderedToolIds.add(callId);
          steps.push({ type: 'tool', toolCall: tc, toolIndex: steps.length });
        }
      });
    });

    // A final tool batch may not have reached executionRounds when the chat
    // stream hands off to TaskRuntime. executionSteps is written at tool_start,
    // so append only calls that the completed rounds did not already project.
    message.executionSteps?.forEach((step) => {
      if (step.type !== 'tool' || renderedToolIds.has(step.callId)) return;
      const tc = message.toolCalls?.find((tool) => tool.id === step.callId);
      if (tc && !isSubagentDispatchTool(tc.name)) {
        renderedToolIds.add(step.callId);
        steps.push({ type: 'tool', toolCall: tc, toolIndex: steps.length });
      }
    });
  } else if (message.executionSteps && message.executionSteps.length > 0) {
    message.executionSteps.forEach((step) => {
      if (step.type === 'thinking') {
        const seg = message.thinkingSegments?.[step.index];
        if (seg && seg.content.trim()) {
          steps.push({ type: 'thinking', thinkingContent: seg.content, toolIndex: 0 });
          thinkingTotal++;
        }
      } else if (step.type === 'tool') {
        const tc = message.toolCalls?.find((tool) => tool.id === step.callId);
        if (tc && !isSubagentDispatchTool(tc.name)) {
          steps.push({ type: 'tool', toolCall: tc, toolIndex: steps.length });
        }
      }
    });
  } else {
    // Fallback: thinkingSegments + toolCalls flat
    const segs = (message.thinkingSegments || []).filter((s) => s.content.trim());
    segs.forEach((s) => {
      steps.push({ type: 'thinking', thinkingContent: s.content, toolIndex: 0 });
      thinkingTotal++;
    });
    if (thinkingTotal === 0 && message.thinkingContent) {
      steps.push({ type: 'thinking', thinkingContent: message.thinkingContent, toolIndex: 0 });
      thinkingTotal++;
    }
    (message.toolCalls || []).forEach((tc, i) => {
      if (!isSubagentDispatchTool(tc.name)) {
        steps.push({ type: 'tool', toolCall: tc, toolIndex: i });
      }
    });
  }
  return { steps, thinkingTotal };
}

export const MessageBubble = memo(function MessageBubble({
  message,
  onRegenerate,
  onEditAndResend,
}: MessageBubbleProps) {
  const isUser = message.role === 'user';
  const [editing, setEditing] = useState(false);
  const [editText, setEditText] = useState(message.content);

  const startEdit = () => {
    setEditText(message.content);
    setEditing(true);
  };
  const cancelEdit = () => {
    setEditing(false);
    setEditText(message.content);
  };
  const submitEdit = () => {
    const trimmed = editText.trim();
    if (!trimmed || trimmed === message.content) {
      cancelEdit();
      return;
    }
    onEditAndResend?.(message.id, trimmed);
    setEditing(false);
  };

  const images = message.attachments?.filter((a) => isImageFile(a.mime_type)) ?? [];
  const files = message.attachments?.filter((a) => !isImageFile(a.mime_type)) ?? [];

  const { steps, thinkingTotal } = flattenSteps(message);
  let thinkingIndex = 0;

  return (
    <div className="flex gap-3 py-3.5">
      {/* Avatar — assistant only; user messages are full-width rows (no bubble) */}
      {!isUser && (
        <div className="flex h-6 w-6 shrink-0 items-center justify-center rounded-md text-xs font-semibold text-[var(--text-tertiary)]">
          <Bot size={14} />
        </div>
      )}

      {/* Content */}
      <div className="min-w-0 flex-1 space-y-2">
        {/* Images */}
        {images.length > 0 && (
          <div className={`grid gap-2 ${images.length === 1 ? 'grid-cols-1' : 'grid-cols-2'}`}>
            {images.map((img, i) => (
              <div
                key={i}
                className="overflow-hidden rounded-lg border border-[var(--border-primary)] bg-[var(--bg-secondary)]"
              >
                <img
                  src={img.url}
                  alt={img.name}
                  className="w-full object-cover"
                  style={{ maxHeight: '300px' }}
                  onClick={() => window.open(img.url, '_blank')}
                />
              </div>
            ))}
          </div>
        )}

        {/* Files */}
        {files.length > 0 && (
          <div className="space-y-1.5">
            {files.map((file, i) => (
              <a
                key={i}
                href={file.url}
                download={file.name}
                className="flex items-center gap-3 rounded-lg border border-[var(--border-primary)] bg-[var(--bg-secondary)] p-3 transition-colors hover:bg-[var(--bg-hover)]"
              >
                <div className="flex h-9 w-9 shrink-0 items-center justify-center rounded-lg bg-[var(--bg-primary)]">
                  <File size={16} className="text-[var(--text-tertiary)]" />
                </div>
                <div className="min-w-0 flex-1">
                  <div className="truncate text-xs font-medium text-[var(--text-primary)]">
                    {file.name}
                  </div>
                  <div className="text-[10px] text-[var(--text-tertiary)]">
                    {formatFileSize(file.size)}
                  </div>
                </div>
                <Download size={14} className="shrink-0 text-[var(--text-tertiary)]" />
              </a>
            ))}
          </div>
        )}

        {/* User prompt reads as a lightweight prompt row, not a content card. */}
        {isUser && message.content && (
          <div className="group/msg relative w-full">
            {!editing && (
              <div className="absolute -top-3 right-0 z-10 flex gap-0.5 rounded-lg border border-[var(--border-primary)] bg-[var(--bg-primary)] px-1 py-0.5 opacity-0 shadow-[var(--shadow-md)] transition-all duration-200 group-hover/msg:opacity-100 group-hover/msg:-translate-y-0.5">
                <ActionButton
                  icon={<Copy size={13} />}
                  label="复制"
                  onClick={() => copyToClipboard(message.content)}
                  copyMode
                />
                <ActionButton icon={<Pencil size={13} />} label="编辑" onClick={startEdit} />
              </div>
            )}
            {editing ? (
              <div className="rounded-lg border-2 border-[var(--accent)] bg-[var(--bg-primary)] px-4 py-3 shadow-[var(--shadow-md)]">
                <textarea
                  value={editText}
                  onChange={(e) => setEditText(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === 'Enter' && !e.shiftKey) {
                      e.preventDefault();
                      submitEdit();
                    }
                    if (e.key === 'Escape') cancelEdit();
                  }}
                  rows={3}
                  className="w-full resize-none bg-transparent text-sm leading-relaxed text-[var(--text-primary)] outline-none"
                  autoFocus
                />
                <div className="mt-3 flex items-center justify-end gap-1.5">
                  <button
                    onClick={cancelEdit}
                    className="flex items-center gap-1 rounded-md px-2.5 py-1 text-xs text-[var(--text-secondary)]"
                  >
                    <X size={12} /> 取消
                  </button>
                  <button
                    onClick={submitEdit}
                    disabled={!editText.trim()}
                    className="flex items-center gap-1 rounded-md px-2.5 py-1 text-xs text-[var(--text-on-accent)]"
                    style={{ background: 'var(--accent)' }}
                  >
                    <ArrowUp size={12} /> 发送
                  </button>
                </div>
              </div>
            ) : (
              <div className="rounded-xl border border-[var(--border-primary)] bg-[var(--bg-primary)] px-4 py-3 text-sm leading-relaxed text-[var(--text-primary)]">
                <div className="whitespace-pre-wrap break-words">{message.content}</div>
              </div>
            )}
          </div>
        )}

        {/* One unified stream: thinking + tools + subagents + final text.
            All peers in a single space-y container — no separate bordered
            sections. This is what makes it read as one continuous flow. */}
        {!isUser && (
          <div className="w-full space-y-2">
            {/* Thinking + tools */}
            {steps.length > 0 && (
              <div className="space-y-1">
                {steps.map((step, i) => {
                  if (step.type === 'thinking') {
                    thinkingIndex++;
                    return (
                      <ThinkingSegment
                        key={`think-${i}`}
                        index={thinkingIndex}
                        total={thinkingTotal}
                        content={step.thinkingContent || ''}
                        isStreaming={message.isStreaming}
                      />
                    );
                  }
                  return (
                    <InlineToolCall
                      key={`tool-${i}`}
                      toolCall={step.toolCall!}
                      index={step.toolIndex}
                    />
                  );
                })}
              </div>
            )}

            {/* Parallel execution segment (run-level subagents) — inline, no wrapper */}
            <ParallelExecutionBlock messageId={message.id} />

            {/* Final text — no left border, plain markdown flow */}
            {message.content && (
              <div className="group/msg relative">
                {!message.isStreaming && !editing && (
                  <div className="absolute -top-3 right-0 z-10 flex gap-0.5 rounded-lg border border-[var(--border-primary)] bg-[var(--bg-primary)] px-1 py-0.5 opacity-0 shadow-[var(--shadow-md)] transition-all duration-200 group-hover/msg:opacity-100 group-hover/msg:-translate-y-0.5">
                    <ActionButton
                      icon={<Copy size={13} />}
                      label="复制"
                      onClick={() => copyToClipboard(message.content)}
                      copyMode
                    />
                    {onRegenerate && (
                      <ActionButton
                        icon={<RefreshCw size={13} />}
                        label="重新生成"
                        onClick={onRegenerate}
                      />
                    )}
                  </div>
                )}
                {editing ? (
                  <div className="rounded-lg border-2 border-[var(--accent)] bg-[var(--bg-primary)] px-4 py-3 shadow-[var(--shadow-md)]">
                    <textarea
                      value={editText}
                      onChange={(e) => setEditText(e.target.value)}
                      onKeyDown={(e) => {
                        if (e.key === 'Enter' && !e.shiftKey) {
                          e.preventDefault();
                          submitEdit();
                        }
                        if (e.key === 'Escape') cancelEdit();
                      }}
                      rows={3}
                      className="w-full resize-none bg-transparent text-sm leading-relaxed text-[var(--text-primary)] outline-none"
                      autoFocus
                    />
                    <div className="mt-3 flex items-center justify-end gap-1.5">
                      <button
                        onClick={cancelEdit}
                        className="flex items-center gap-1 rounded-md px-2.5 py-1 text-xs text-[var(--text-secondary)]"
                      >
                        <X size={12} /> 取消
                      </button>
                      <button
                        onClick={submitEdit}
                        disabled={!editText.trim()}
                        className="flex items-center gap-1 rounded-md px-2.5 py-1 text-xs text-[var(--text-on-accent)]"
                        style={{ background: 'var(--accent)' }}
                      >
                        <ArrowUp size={12} /> 发送
                      </button>
                    </div>
                  </div>
                ) : (
                  <div className="text-sm leading-relaxed text-[var(--text-assistant-msg)]">
                    <MarkdownContent className="break-words" content={message.content} />
                    {message.isStreaming && (
                      <span className="ml-0.5 inline-block h-[14px] w-[3px] animate-pulse rounded-full bg-[var(--accent)] align-text-bottom" />
                    )}
                  </div>
                )}
              </div>
            )}
          </div>
        )}
      </div>
    </div>
  );
});

function ActionButton({
  icon,
  label,
  onClick,
  copyMode,
}: {
  icon: React.ReactNode;
  label: string;
  onClick: () => void;
  copyMode?: boolean;
}) {
  const [done, setDone] = useState(false);
  const handleClick = (e: React.MouseEvent) => {
    e.stopPropagation();
    onClick();
    if (copyMode) {
      setDone(true);
      setTimeout(() => setDone(false), 2000);
    }
  };
  return (
    <button
      onClick={handleClick}
      className={`flex items-center gap-1 rounded-md px-1.5 py-1 text-[11px] transition-colors ${done ? 'text-[var(--color-success)]' : 'text-[var(--text-tertiary)] hover:text-[var(--text-primary)]'}`}
      title={label}
    >
      {done ? <Check size={13} /> : icon}
    </button>
  );
}
