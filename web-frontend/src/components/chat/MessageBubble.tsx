import { useState, memo } from 'react';
import type { ChatMessage } from '../../types/api';
import { ToolCallCard } from './ToolCallCard';
import { ChartCard } from './ChartCard';
import {
  User,
  Bot,
  Copy,
  Check,
  RefreshCw,
  Pencil,
  X,
  ArrowUp,
  File,
  Download,
  ChevronDown,
  ChevronRight,
  Brain,
} from 'lucide-react';
import MarkdownContent from '../common/MarkdownContent';

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
  // Fallback for non-HTTPS contexts
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

  return (
    <div className={`flex gap-3 py-3.5 ${isUser ? 'flex-row-reverse' : ''}`}>
      {/* Avatar */}
      <div
        className={`flex h-7 w-7 shrink-0 items-center justify-center rounded-lg text-xs font-semibold
          ${
            isUser
              ? 'bg-[var(--bg-user-msg)] text-[var(--text-user-msg)]'
              : 'border border-[var(--border-primary)] bg-[var(--bg-secondary)] text-[var(--text-secondary)]'
          }`}
      >
        {isUser ? <User size={14} /> : <Bot size={14} />}
      </div>

      {/* Content */}
      <div className={`min-w-0 space-y-2 ${isUser ? 'max-w-[72%] items-end' : 'w-full max-w-[92%]'}`}>
        {/* Execution process — grouped thinking + tool calls in chronological order */}
        {!isUser && (
          <ExecutionProcessBlock
            thinkingSegments={message.thinkingSegments || []}
            thinkingContent={message.thinkingContent}
            toolCalls={message.toolCalls || []}
            executionSteps={message.executionSteps || []}
            executionRounds={message.executionRounds}
            isStreaming={message.isStreaming}
          />
        )}

        {/* Chart specs */}
        {message.chartSpecs && message.chartSpecs.length > 0 && (
          <div className="space-y-2">
            {message.chartSpecs.map((spec, i) => (
              <ChartCard key={i} spec={spec} standalone />
            ))}
          </div>
        )}

        {/* Attachments - Images */}
        {images.length > 0 && (
          <div className={`grid gap-2 ${images.length === 1 ? 'grid-cols-1' : 'grid-cols-2'}`}>
            {images.map((img, i) => (
              <div
                key={i}
                className="overflow-hidden rounded-xl border border-[var(--border-primary)] bg-[var(--bg-secondary)]"
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

        {/* Attachments - Files */}
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

        {message.content && (
          <div className="group/msg relative">
            {/* Action buttons on hover */}
            {!message.isStreaming && !editing && (
              <div
                className={`absolute -top-3 z-10 flex gap-0.5 rounded-lg border border-[var(--border-primary)] bg-[var(--bg-primary)] px-1 py-0.5 opacity-0 shadow-[var(--shadow-md)] transition-all duration-200 group-hover/msg:opacity-100 group-hover/msg:-translate-y-0.5
                  ${isUser ? 'left-0' : 'right-0'}`}
              >
                <ActionButton
                  icon={<Copy size={13} />}
                  label="复制"
                  onClick={() => copyToClipboard(message.content)}
                  copyMode
                />
                {isUser && (
                  <ActionButton icon={<Pencil size={13} />} label="编辑" onClick={startEdit} />
                )}
                {!isUser && onRegenerate && (
                  <ActionButton
                    icon={<RefreshCw size={13} />}
                    label="重新生成"
                    onClick={onRegenerate}
                  />
                )}
              </div>
            )}

            {editing ? (
              <div className="rounded-2xl border-2 border-[var(--accent)] bg-[var(--bg-primary)] px-4 py-3 shadow-[var(--shadow-md)]">
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
              <div
                className={`text-sm leading-relaxed
                  ${
                    isUser
                      ? 'rounded-2xl bg-[var(--bg-user-msg)] px-4 py-2.5 text-[var(--text-user-msg)]'
                      : 'border-l-2 border-[var(--border-primary)] px-4 py-1 text-[var(--text-assistant-msg)] md-content'
                  }`}
              >
                {isUser ? (
                  <div className="whitespace-pre-wrap break-words">{message.content}</div>
                ) : (
                  <MarkdownContent
                    className="break-words"
                    content={message.content}
                  />
                )}
                {message.isStreaming && (
                  <span className="ml-0.5 inline-block h-[14px] w-[3px] animate-pulse rounded-full bg-[var(--accent)] align-text-bottom" />
                )}
              </div>
            )}
          </div>
        )}
      </div>
    </div>
  );
});

// ── Execution Process Block (groups thinking + tools chronologically) ─────────────

interface ExecutionStep {
  type: 'thinking' | 'tool';
  index: number;
  content?: string;
  toolCall?: any;
}

function ExecutionProcessBlock({
  thinkingSegments,
  thinkingContent,
  toolCalls,
  executionSteps,
  executionRounds,
  isStreaming,
}: {
  thinkingSegments: Array<{ content: string }>;
  thinkingContent?: string;
  toolCalls: Array<any>;
  executionSteps: Array<{ type: 'thinking' | 'tool'; index: number }>;
  executionRounds?: Array<{ thinking?: { content: string }; tools: Array<any> }>;
  isStreaming?: boolean;
}) {
  const [expanded, setExpanded] = useState(true);

  // Build chronological execution steps using executionSteps order
  const steps: ExecutionStep[] = [];
  let totalThinking = 0;
  let totalTools = 0;

  if (executionRounds && executionRounds.length > 0) {
    // New round-based model: each round has thinking + tools (parallel tools grouped)
    executionRounds.forEach((round, ri) => {
      if (round.thinking && round.thinking.content.trim()) {
        steps.push({ type: 'thinking', index: ri, content: round.thinking.content });
        totalThinking++;
      }
      round.tools.forEach((tc, ti) => {
        steps.push({ type: 'tool', index: ri * 1000 + ti, toolCall: tc });
        totalTools++;
      });
    });
  } else if (executionSteps && executionSteps.length > 0) {
    // Use the recorded execution order (legacy flat model)
    executionSteps.forEach((step) => {
      if (step.type === 'thinking') {
        const segment = thinkingSegments[step.index];
        if (segment && segment.content.trim()) {
          steps.push({ type: 'thinking', index: step.index, content: segment.content });
        }
      } else if (step.type === 'tool') {
        const toolCall = toolCalls[step.index];
        if (toolCall) {
          steps.push({ type: 'tool', index: step.index, toolCall });
        }
      }
    });
    totalThinking = steps.filter((s) => s.type === 'thinking').length;
    totalTools = steps.filter((s) => s.type === 'tool').length;
  } else {
    // Fallback for legacy messages without executionSteps
    const nonEmptyThinking = thinkingSegments.filter((seg) => seg.content.trim());
    if (nonEmptyThinking.length > 0) {
      nonEmptyThinking.forEach((seg, i) => {
        steps.push({ type: 'thinking', index: i, content: seg.content });
      });
    } else if (thinkingContent) {
      steps.push({ type: 'thinking', index: 0, content: thinkingContent });
    }
    toolCalls.forEach((tc, i) => {
      steps.push({ type: 'tool', index: i, toolCall: tc });
    });
    totalThinking = nonEmptyThinking.length || (thinkingContent ? 1 : 0);
    totalTools = toolCalls.length;
  }

  // If no steps, don't render anything
  if (steps.length === 0) return null;

  const thinkingCount = totalThinking;
  const toolCount = totalTools;

  const summary = [];
  if (thinkingCount > 0) summary.push(`${thinkingCount} 思考`);
  if (toolCount > 0) summary.push(`${toolCount} 工具`);

  const label = `思考与执行 (${summary.join(', ')})`;

  return (
    <div
      className="my-1 min-w-0 overflow-hidden rounded-lg border border-[var(--border-primary)] bg-[var(--bg-secondary)]"
      style={{
        borderLeft: `2px solid ${isStreaming ? 'var(--color-purple)' : 'var(--border-primary)'}`,
      }}
    >
      <button
        onClick={() => setExpanded(!expanded)}
        className="flex w-full items-center gap-2 px-3 py-1.5 text-left transition-colors hover:bg-[var(--bg-hover)]"
      >
        <Brain
          size={12}
          className={`shrink-0 ${isStreaming ? 'text-[var(--color-purple)] animate-pulse' : 'text-[var(--text-tertiary)]'}`}
        />
        <span
          className={`text-xs ${isStreaming ? 'text-[var(--color-purple)] font-medium' : 'text-[var(--text-secondary)]'}`}
        >
          {label}
          {isStreaming && !expanded && '...'}
        </span>
        {isStreaming && !expanded && (
          <span className="ml-1 inline-block h-1.5 w-1.5 animate-pulse rounded-full bg-[var(--color-purple)]" />
        )}
        <span className="ml-auto">
          {expanded ? (
            <ChevronDown size={12} className="text-[var(--text-tertiary)]" />
          ) : (
            <ChevronRight size={12} className="text-[var(--text-tertiary)]" />
          )}
        </span>
      </button>

      {expanded && (
        <div className="border-t border-[var(--border-primary)] px-3 pb-2 pt-2 space-y-1.5 max-h-80 overflow-y-auto">
          {steps.map((step, i) => {
            if (step.type === 'thinking') {
              return (
                <div
                  key={`thinking-${step.index}`}
                  className="rounded border-l-2 border-[var(--color-purple)] bg-[var(--bg-primary)] px-2 py-1.5"
                >
                  <div className="mb-1 flex items-center gap-1.5">
                    <Brain size={10} className="text-[var(--color-purple)]" />
                    <span className="text-[10px] font-medium text-[var(--color-purple)]">
                      思考 {thinkingCount > 1 ? `${i + 1}/${thinkingCount}` : ''}
                    </span>
                  </div>
                  <div className="max-h-48 overflow-y-auto text-xs leading-relaxed text-[var(--text-secondary)] whitespace-pre-wrap break-words">
                    {step.content}
                  </div>
                </div>
              );
            } else {
              return <ToolCallCard key={`tool-${step.index}`} toolCall={step.toolCall} compact />;
            }
          })}
        </div>
      )}
    </div>
  );
}

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
      className={`flex items-center gap-1 rounded-md px-1.5 py-1 text-[11px] transition-colors
        ${done ? 'text-[var(--color-success)]' : 'text-[var(--text-tertiary)] hover:text-[var(--text-primary)]'}`}
      title={label}
    >
      {done ? <Check size={13} /> : icon}
    </button>
  );
}
