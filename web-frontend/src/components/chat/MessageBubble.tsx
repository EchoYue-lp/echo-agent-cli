import { useState, useEffect, memo } from 'react';
import type { ChatMessage } from '../../types/api';
import { ToolCallCard } from './ToolCallCard';
import { ChartCard } from './ChartCard';
import { User, Bot, Copy, Check, RefreshCw, Pencil, X, ArrowUp, File, Download, ChevronDown, ChevronRight, Brain } from 'lucide-react';
import { renderMarkdown } from '../../utils/markdown';

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

export const MessageBubble = memo(function MessageBubble({ message, onRegenerate, onEditAndResend }: MessageBubbleProps) {
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
    <div className={`flex gap-3 py-3 ${isUser ? 'flex-row-reverse' : ''}`}>
      {/* Avatar */}
      <div
        className={`flex h-8 w-8 shrink-0 items-center justify-center rounded-xl text-xs font-semibold
          ${isUser
            ? 'bg-[var(--accent)] text-white'
            : 'border border-[var(--border-primary)] bg-[var(--bg-primary)] text-[var(--text-secondary)]'}`}
      >
        {isUser ? <User size={14} /> : <Bot size={14} />}
      </div>

      {/* Content */}
      <div className={`min-w-0 max-w-[80%] space-y-2 ${isUser ? 'items-end' : ''}`}>
        {/* Thinking process — one block per ReAct iteration */}
        {!isUser && (
          <>
            {message.thinkingSegments && message.thinkingSegments.length > 0
              ? (() => {
                  const nonEmpty = message.thinkingSegments.filter((seg) => seg.content.trim());
                  if (nonEmpty.length === 0) return null;
                  return nonEmpty.map((seg, i) => (
                    <ThinkingBlock
                      key={i}
                      index={i + 1}
                      total={nonEmpty.length}
                      content={seg.content}
                      isStreaming={message.isStreaming && i === nonEmpty.length - 1}
                    />
                  ));
                })()
              : message.thinkingContent && (
                  // Legacy: old messages with single thinkingContent string
                  <ThinkingBlock index={1} total={1} content={message.thinkingContent} />
                )
            }
          </>
        )}

        {/* Tool calls */}
        {message.toolCalls && message.toolCalls.length > 0 && (
          <div className="space-y-1.5">
            {message.toolCalls.map((tc, i) => (
              <ToolCallCard key={i} toolCall={tc} />
            ))}
          </div>
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
                <ActionButton icon={<Copy size={13} />} label="复制" onClick={() => copyToClipboard(message.content)} copyMode />
                {isUser && (
                  <ActionButton icon={<Pencil size={13} />} label="编辑" onClick={startEdit} />
                )}
                {!isUser && onRegenerate && (
                  <ActionButton icon={<RefreshCw size={13} />} label="重新生成" onClick={onRegenerate} />
                )}
              </div>
            )}

            {editing ? (
              <div className="rounded-2xl border-2 border-[var(--accent)] bg-[var(--bg-primary)] px-4 py-3 shadow-[var(--shadow-md)]">
                <textarea
                  value={editText}
                  onChange={(e) => setEditText(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); submitEdit(); }
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
                    className="flex items-center gap-1 rounded-md px-2.5 py-1 text-xs text-white"
                    style={{ background: 'var(--accent)' }}
                  >
                    <ArrowUp size={12} /> 发送
                  </button>
                </div>
              </div>
            ) : (
              <div
                className={`rounded-2xl px-4 py-2.5 text-sm leading-relaxed
                  ${isUser
                    ? 'bg-[var(--accent)] text-white'
                    : 'border border-[var(--border-primary)] bg-[var(--bg-primary)] text-[var(--text-assistant-msg)] md-content'}`}
              >
                {isUser ? (
                  <div className="whitespace-pre-wrap break-words">{message.content}</div>
                ) : (
                  <div className="break-words" dangerouslySetInnerHTML={{ __html: renderMarkdown(message.content) }} />
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

function ThinkingBlock({ content, index, total, isStreaming }: { content: string; index: number; total: number; isStreaming?: boolean }) {
  const [expanded, setExpanded] = useState(isStreaming ?? false);
  const [manualToggle, setManualToggle] = useState(false);

  // Auto-expand during streaming, auto-collapse when done (unless user manually toggled)
  useEffect(() => {
    if (!manualToggle) {
      setExpanded(!!isStreaming);
    }
  }, [isStreaming, manualToggle]);

  const handleToggle = () => {
    setManualToggle(true);
    setExpanded((prev) => !prev);
  };

  const label = total > 1 ? `思考过程 ${index}/${total}` : '思考过程';
  const isActive = isStreaming && expanded;

  return (
    <div className="my-1 overflow-hidden rounded-lg border-l-2 border-[var(--border-primary)] bg-[var(--bg-secondary)]"
      style={{ borderLeftColor: isStreaming ? 'var(--color-purple)' : 'var(--border-primary)' }}>
      <button
        onClick={handleToggle}
        className="flex w-full items-center gap-2 px-3 py-2 text-left transition-colors hover:bg-[var(--bg-hover)]"
      >
        <Brain size={13} className={`shrink-0 ${isStreaming ? 'text-[var(--color-purple)]' : 'text-[var(--text-tertiary)]'}`} />
        <span className={`text-xs font-medium ${isStreaming ? 'text-[var(--color-purple)]' : 'text-[var(--text-secondary)]'}`}>
          {label}
        </span>
        {isStreaming && !expanded && (
          <span className="ml-1 inline-block h-2 w-2 animate-pulse rounded-full bg-[var(--color-purple)]" />
        )}
        <span className="ml-auto">
          {expanded
            ? <ChevronDown size={14} className="text-[var(--text-tertiary)]" />
            : <ChevronRight size={14} className="text-[var(--text-tertiary)]" />
          }
        </span>
      </button>
      {expanded && (
        <div className="border-t border-[var(--border-primary)] px-3 pb-3 pt-2">
          <div className="max-h-72 overflow-y-auto rounded-lg bg-[var(--bg-primary)] p-3 text-xs leading-relaxed text-[var(--text-secondary)] whitespace-pre-wrap break-words">
            {content}
            {isActive && (
              <span className="ml-0.5 inline-block h-3 w-[2px] animate-pulse rounded-full bg-[var(--color-purple)] align-text-bottom" />
            )}
          </div>
        </div>
      )}
    </div>
  );
}

function ActionButton({ icon, label, onClick, copyMode }: {
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
