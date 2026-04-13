import { useState, useRef, useEffect } from 'react';
import { ArrowUp, Square } from 'lucide-react';

interface ChatInputProps {
  onSend: (text: string) => void;
  isStreaming?: boolean;
  onCancel?: () => void;
}

export function ChatInput({ onSend, isStreaming, onCancel }: ChatInputProps) {
  const [text, setText] = useState('');
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  // Auto-grow textarea
  useEffect(() => {
    const el = textareaRef.current;
    if (!el) return;
    el.style.height = 'auto';
    el.style.height = Math.min(el.scrollHeight, 200) + 'px';
  }, [text]);

  const handleSend = () => {
    const trimmed = text.trim();
    if (!trimmed) return;
    onSend(trimmed);
    setText('');
    requestAnimationFrame(() => {
      if (textareaRef.current) {
        textareaRef.current.style.height = 'auto';
      }
    });
  };

  const handleKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      handleSend();
    }
  };

  return (
    <div className="px-4 pb-4 pt-2">
      <div
        className="mx-auto flex max-w-3xl items-end rounded-2xl px-4 py-3 transition-shadow focus-within:shadow-md"
        style={{
          background: 'var(--bg-input)',
          border: '1px solid var(--border-primary)',
          boxShadow: '0 0 0 0 transparent',
        }}
      >
        <textarea
          ref={textareaRef}
          value={text}
          onChange={(e) => setText(e.target.value)}
          onKeyDown={handleKeyDown}
          rows={1}
          placeholder="Message Echo Agent..."
          className="flex-1 resize-none bg-transparent text-[14px] outline-none"
          style={{
            color: 'var(--text-primary)',
            minHeight: '24px',
            maxHeight: '200px',
            lineHeight: '1.5',
          }}
        />
        {isStreaming ? (
          <button
            onClick={onCancel}
            className="ml-2 flex h-8 w-8 shrink-0 items-center justify-center rounded-lg transition-opacity hover:opacity-80"
            style={{ background: 'var(--text-secondary)' }}
            title="Stop generating"
          >
            <Square size={14} fill="white" color="white" />
          </button>
        ) : (
          <button
            onClick={handleSend}
            disabled={!text.trim()}
            className="ml-2 flex h-8 w-8 shrink-0 items-center justify-center rounded-lg transition-all disabled:opacity-20"
            style={{ background: 'var(--accent)' }}
          >
            <ArrowUp size={16} color="white" strokeWidth={2.5} />
          </button>
        )}
      </div>
      <p className="mx-auto mt-2 max-w-3xl text-center text-[11px]" style={{ color: 'var(--text-tertiary)' }}>
        Echo Agent can make mistakes. Consider verifying important information.
      </p>
    </div>
  );
}
