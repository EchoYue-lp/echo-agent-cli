import { useState } from 'react';
import type { ChatMessage } from '../../types/api';
import { ToolCallCard } from './ToolCallCard';
import { User, Bot, Copy, Check, RefreshCw, Pencil, X, ArrowUp } from 'lucide-react';
import { renderMarkdown } from '../../utils/markdown';

interface MessageBubbleProps {
  message: ChatMessage;
  onRegenerate?: () => void;
  onEditAndResend?: (messageId: string, newContent: string) => void;
}

export function MessageBubble({ message, onRegenerate, onEditAndResend }: MessageBubbleProps) {
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

  return (
    <div className={`flex gap-3 py-3 ${isUser ? 'flex-row-reverse' : ''}`}>
      {/* Avatar */}
      <div
        className="flex h-8 w-8 shrink-0 items-center justify-center rounded-full text-xs font-semibold"
        style={{
          background: isUser ? 'var(--accent)' : 'var(--bg-hover)',
          color: isUser ? '#fff' : 'var(--text-secondary)',
        }}
      >
        {isUser ? <User size={14} /> : <Bot size={14} />}
      </div>

      {/* Content area */}
      <div className={`min-w-0 max-w-[80%] space-y-2 ${isUser ? 'items-end' : ''}`}>
        {/* Tool calls */}
        {message.toolCalls && message.toolCalls.length > 0 && (
          <div className="space-y-1.5">
            {message.toolCalls.map((tc, i) => (
              <ToolCallCard key={i} toolCall={tc} />
            ))}
          </div>
        )}

        {/* Text content */}
        {message.content && (
          <div className="relative group/msg">
            {/* Action buttons — appear on hover */}
            {!message.isStreaming && !editing && (
              <div
                className={`absolute -top-3 ${isUser ? 'left-0' : 'right-0'} z-10 flex gap-0.5 rounded-lg px-1 py-0.5 opacity-0 transition-opacity group-hover/msg:opacity-100`}
                style={{ background: 'var(--bg-primary)', border: '1px solid var(--border-primary)', boxShadow: 'var(--shadow-sm)' }}
              >
                <ActionButton icon={<Copy size={13} />} label="Copy" onClick={() => navigator.clipboard.writeText(message.content)} copyMode />
                {isUser && (
                  <ActionButton icon={<Pencil size={13} />} label="Edit" onClick={startEdit} />
                )}
                {!isUser && onRegenerate && (
                  <ActionButton icon={<RefreshCw size={13} />} label="Regenerate" onClick={onRegenerate} />
                )}
              </div>
            )}

            {editing ? (
              /* Edit mode */
              <div
                className="rounded-2xl border-2 px-3 py-2"
                style={{ borderColor: 'var(--accent)', background: 'var(--bg-primary)' }}
              >
                <textarea
                  value={editText}
                  onChange={(e) => setEditText(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); submitEdit(); }
                    if (e.key === 'Escape') cancelEdit();
                  }}
                  rows={3}
                  className="w-full resize-none bg-transparent text-[14px] leading-relaxed outline-none"
                  style={{ color: 'var(--text-primary)' }}
                  autoFocus
                />
                <div className="mt-2 flex items-center justify-end gap-1.5">
                  <button
                    onClick={cancelEdit}
                    className="flex items-center gap-1 rounded-md px-2.5 py-1 text-xs"
                    style={{ color: 'var(--text-secondary)' }}
                  >
                    <X size={12} /> Cancel
                  </button>
                  <button
                    onClick={submitEdit}
                    disabled={!editText.trim()}
                    className="flex items-center gap-1 rounded-md px-2.5 py-1 text-xs text-white disabled:opacity-30"
                    style={{ background: 'var(--accent)' }}
                  >
                    <ArrowUp size={12} /> Send
                  </button>
                </div>
              </div>
            ) : (
              /* Normal display */
              <div
                className={`rounded-2xl px-4 py-2.5 text-[14px] leading-relaxed ${isUser ? '' : 'md-content'}`}
                style={{
                  background: isUser ? 'var(--accent)' : 'var(--bg-primary)',
                  color: isUser ? '#fff' : 'var(--text-primary)',
                  border: isUser ? 'none' : '1px solid var(--border-primary)',
                }}
              >
                {isUser ? (
                  <div className="whitespace-pre-wrap break-words">{message.content}</div>
                ) : (
                  <div className="break-words" dangerouslySetInnerHTML={{ __html: renderMarkdown(message.content) }} />
                )}
                {message.isStreaming && (
                  <span
                    className="ml-0.5 inline-block h-[14px] w-[3px] animate-pulse rounded-full align-text-bottom"
                    style={{ background: 'var(--accent)' }}
                  />
                )}
              </div>
            )}
          </div>
        )}
      </div>
    </div>
  );
}

/* ── Action Button ── */

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
      className="flex items-center gap-1 rounded-md px-1.5 py-1 text-[11px] transition-colors"
      style={{ color: done ? '#10b981' : 'var(--text-tertiary)' }}
      title={label}
    >
      {done ? <Check size={13} /> : icon}
    </button>
  );
}
