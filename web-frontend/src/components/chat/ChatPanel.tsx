import { useRef, useEffect, useCallback } from 'react';
import { useChatStore } from '../../stores/chatStore';
import { MessageBubble } from './MessageBubble';
import { ApprovalCard } from './ApprovalCard';
import { ChatInput } from './ChatInput';
import { WelcomeScreen } from './WelcomeScreen';
import { useWebSocket } from '../../hooks/useWebSocket';

export function ChatPanel() {
  const messages = useChatStore((s) => s.messages);
  const approvalRequest = useChatStore((s) => s.approvalRequest);
  const inputRequest = useChatStore((s) => s.inputRequest);
  const isStreaming = useChatStore((s) => s.isStreaming);
  const isCancelled = useChatStore((s) => s.isCancelled);
  const scrollRef = useRef<HTMLDivElement>(null);
  const bottomRef = useRef<HTMLDivElement>(null);
  const isNearBottomRef = useRef(true);
  const { sendMessage, sendApproval, sendInput, cancel, connectionStatus } = useWebSocket();

  const handleRegenerate = () => {
    const store = useChatStore.getState();
    const content = store.prepareRegenerate();
    if (content) sendMessage(content);
  };

  const handleEditAndResend = (messageId: string, newContent: string) => {
    const store = useChatStore.getState();
    const content = store.prepareEditAndResend(messageId, newContent);
    if (content) sendMessage(content);
  };

  // Track whether user is near the bottom of the scroll container
  const handleScroll = useCallback(() => {
    const el = scrollRef.current;
    if (!el) return;
    const threshold = 120;
    isNearBottomRef.current = el.scrollHeight - el.scrollTop - el.clientHeight < threshold;
  }, []);

  // Smart auto-scroll: only scroll when user is near the bottom
  useEffect(() => {
    if (isNearBottomRef.current && bottomRef.current) {
      bottomRef.current.scrollIntoView({ behavior: isStreaming ? 'auto' : 'smooth' });
    }
  }, [messages, approvalRequest, inputRequest, isStreaming]);

  const handleSuggestionClick = (text: string) => {
    sendMessage(text);
  };

  return (
    <div className="flex h-full flex-col" style={{ background: 'var(--bg-chat)' }}>
      {/* Scrollable messages area */}
      <div ref={scrollRef} className="flex-1 overflow-y-auto" onScroll={handleScroll}>
        {messages.length === 0 ? (
          <WelcomeScreen onSuggestionClick={handleSuggestionClick} />
        ) : (
          <div className="mx-auto max-w-3xl px-4 sm:px-6">
            {/* Messages */}
            <div className="space-y-1 pb-4">
              {messages.map((msg, idx) => {
                // Check if we need a date separator between messages
                const prevMsg = idx > 0 ? messages[idx - 1] : null;
                const showSeparator = idx === 0 || (prevMsg && msg.timestamp && prevMsg.timestamp && msg.timestamp - prevMsg.timestamp > 300000);

                return (
                  <div key={msg.id}>
                    {showSeparator && idx > 0 && (
                      <div className="flex items-center gap-3 py-4">
                        <div className="h-px flex-1" style={{ background: 'var(--border-primary)' }} />
                        <span className="text-xs" style={{ color: 'var(--text-tertiary)' }}>
                          {new Date(msg.timestamp).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })}
                        </span>
                        <div className="h-px flex-1" style={{ background: 'var(--border-primary)' }} />
                      </div>
                    )}
                    <MessageBubble message={msg} onRegenerate={handleRegenerate} onEditAndResend={handleEditAndResend} />
                  </div>
                );
              })}

              {/* Streaming thinking indicator */}
              {isStreaming && !messages.some((m) => m.isStreaming && m.content) && (
                <div className="flex items-center gap-2 px-1 py-2">
                  <div className="spinner" />
                  <span className="text-xs" style={{ color: 'var(--text-tertiary)' }}>Thinking...</span>
                </div>
              )}

              {/* Cancelled indicator */}
              {isCancelled && (
                <div className="flex items-center gap-3 py-2">
                  <div className="h-px flex-1" style={{ background: 'var(--border-primary)' }} />
                  <span className="text-xs" style={{ color: 'var(--text-tertiary)' }}>Response stopped</span>
                  <div className="h-px flex-1" style={{ background: 'var(--border-primary)' }} />
                </div>
              )}

              {/* Approval request */}
              {approvalRequest && (
                <div className="py-2">
                  <ApprovalCard
                    request={approvalRequest}
                    onApprove={() => sendApproval(approvalRequest.requestId, true)}
                    onReject={(reason) => sendApproval(approvalRequest.requestId, false, reason)}
                  />
                </div>
              )}

              {/* Input request */}
              {inputRequest && (
                <div className="py-2">
                  <InputCard
                    prompt={inputRequest.prompt}
                    onSubmit={(text) => sendInput(inputRequest.requestId, text)}
                  />
                </div>
              )}
            </div>
            <div ref={bottomRef} className="h-1" />
          </div>
        )}
      </div>

      {/* Connection status bar */}
      {connectionStatus === 'disconnected' && (
        <div
          className="flex items-center justify-center gap-2 px-4 py-1.5 text-xs font-medium"
          style={{
            background: '#ef444415',
            color: '#fef2f2',
          }}
        >
          <span className="inline-block h-2 w-2 rounded-full" style={{ background: '#fca5a5' }} />
          Disconnected — reconnecting...
        </div>
      )}

      {/* Bottom bar: stop button + input */}
      <div style={{ background: 'var(--bg-chat)' }}>
        {/* Floating stop button */}
        {isStreaming && messages.length > 0 && (
          <div className="flex justify-center pb-2">
            <button
              onClick={cancel}
              className="flex items-center gap-2 rounded-full px-4 py-2 text-sm font-medium transition-all hover:opacity-80"
              style={{
                background: 'var(--bg-primary)',
                color: 'var(--text-secondary)',
                border: '1px solid var(--border-primary)',
                boxShadow: 'var(--shadow-md)',
              }}
            >
              <div className="h-3 w-3 rounded-[2px]" style={{ background: 'var(--text-secondary)' }} />
              Stop generating
            </button>
          </div>
        )}

        {/* Input area */}
        <ChatInput
          onSend={sendMessage}
          isStreaming={isStreaming}
          onCancel={cancel}
        />
      </div>
    </div>
  );
}

function InputCard({ prompt, onSubmit }: { prompt?: string; onSubmit: (text: string) => void }) {
  const handleSubmit = (e: React.FormEvent<HTMLFormElement>) => {
    e.preventDefault();
    const form = new FormData(e.currentTarget);
    const text = form.get('input') as string;
    if (text.trim()) {
      onSubmit(text);
      e.currentTarget.reset();
    }
  };

  return (
    <div
      className="animate-pulse-border rounded-xl border-2 p-4"
      style={{
        background: 'var(--bg-primary)',
        borderColor: 'var(--accent)',
      }}
    >
      {prompt && (
        <p className="mb-2 text-sm font-medium" style={{ color: 'var(--text-primary)' }}>{prompt}</p>
      )}
      <form onSubmit={handleSubmit} className="flex gap-2">
        <input
          name="input"
          className="flex-1 rounded-lg px-3 py-2 text-sm outline-none"
          style={{ border: '1px solid var(--border-primary)', background: 'var(--bg-input)', color: 'var(--text-primary)' }}
          placeholder="Type your response..."
          autoFocus
        />
        <button
          type="submit"
          className="rounded-lg px-4 py-2 text-sm font-medium text-white"
          style={{ background: 'var(--accent)' }}
        >
          Respond
        </button>
      </form>
    </div>
  );
}
