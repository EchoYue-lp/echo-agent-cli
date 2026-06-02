import { useRef, useEffect, useCallback } from 'react';
import { useChatStore } from '../../stores/chatStore';
import { MessageBubble } from './MessageBubble';
import { ApprovalCard } from './ApprovalCard';
import { ChatInput } from './ChatInput';
import { WelcomeScreen } from './WelcomeScreen';
import { useWebSocket } from '../../hooks/useWebSocket';
import { useTauriChat } from '../../hooks/useTauriChat';
import { isTauri } from '../../lib/tauri-bridge';
import type { Attachment } from '../../types/api';

function useChatTransport() {
  const ws = useWebSocket();
  const tauri = useTauriChat();
  return isTauri() ? tauri : ws;
}

export function ChatPanel() {
  const messages = useChatStore((s) => s.messages);
  const approvalRequest = useChatStore((s) => s.approvalRequest);
  const inputRequest = useChatStore((s) => s.inputRequest);
  const isStreaming = useChatStore((s) => s.isStreaming);
  const isCancelled = useChatStore((s) => s.isCancelled);
  const scrollRef = useRef<HTMLDivElement>(null);
  const bottomRef = useRef<HTMLDivElement>(null);
  const isNearBottomRef = useRef(true);
  const { sendMessage, sendApproval, sendInput, cancel, connectionStatus } = useChatTransport();

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

  const handleScroll = useCallback(() => {
    const el = scrollRef.current;
    if (!el) return;
    const nearBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 120;
    isNearBottomRef.current = nearBottom;
  }, []);

  useEffect(() => {
    if (isNearBottomRef.current && bottomRef.current) {
      bottomRef.current.scrollIntoView({ behavior: isStreaming ? 'auto' : 'smooth' });
    }
  }, [messages, approvalRequest, inputRequest, isStreaming]);

  const handleSuggestionClick = (text: string) => {
    sendMessage(text);
  };

  const handleSend = (text: string, attachments?: Attachment[]) => {
    sendMessage(text, attachments);
  };

  return (
    <div className="flex h-full flex-col min-h-0" role="main" aria-label="聊天面板">
      {/* Messages area */}
      <div
        ref={scrollRef}
        className="flex-1 overflow-y-auto min-h-0"
        onScroll={handleScroll}
        role="log"
        aria-live="polite"
        aria-label="消息列表"
      >
        {messages.length === 0 ? (
          <WelcomeScreen onSuggestionClick={handleSuggestionClick} />
        ) : (
          <div className="mx-auto max-w-3xl px-4 sm:px-6">
            <div className="space-y-2 pb-4 pt-2">
              {messages.map((msg, idx) => {
                const prevMsg = idx > 0 ? messages[idx - 1] : null;
                const showSeparator =
                  idx === 0 ||
                  (prevMsg &&
                    msg.timestamp &&
                    prevMsg.timestamp &&
                    msg.timestamp - prevMsg.timestamp > 300000);

                return (
                  <div key={msg.id}>
                    {showSeparator && idx > 0 && (
                      <div className="flex items-center gap-3 py-4">
                        <div
                          className="h-px flex-1"
                          style={{
                            background:
                              'linear-gradient(to right, transparent, var(--border-primary), transparent)',
                          }}
                        />
                        <span
                          className="text-xs text-[var(--text-tertiary)]"
                          style={{ fontVariantNumeric: 'tabular-nums' }}
                        >
                          {new Date(msg.timestamp).toLocaleTimeString([], {
                            hour: '2-digit',
                            minute: '2-digit',
                          })}
                        </span>
                        <div
                          className="h-px flex-1"
                          style={{
                            background:
                              'linear-gradient(to right, transparent, var(--border-primary), transparent)',
                          }}
                        />
                      </div>
                    )}
                    <MessageBubble
                      message={msg}
                      onRegenerate={handleRegenerate}
                      onEditAndResend={handleEditAndResend}
                    />
                  </div>
                );
              })}

              {isStreaming &&
                !messages.some(
                  (m) =>
                    m.isStreaming &&
                    (m.content || (m.thinkingSegments && m.thinkingSegments.length > 0))
                ) && (
                  <div className="flex items-center gap-3 px-1 py-3">
                    <div className="spinner" />
                    <div className="flex items-center gap-1.5">
                      <span className="text-xs text-[var(--text-tertiary)] animate-breathe">
                        思考中
                      </span>
                      <span className="flex gap-0.5">
                        <span
                          className="h-1 w-1 rounded-full bg-[var(--text-tertiary)] animate-bounce"
                          style={{ animationDelay: '0ms' }}
                        />
                        <span
                          className="h-1 w-1 rounded-full bg-[var(--text-tertiary)] animate-bounce"
                          style={{ animationDelay: '150ms' }}
                        />
                        <span
                          className="h-1 w-1 rounded-full bg-[var(--text-tertiary)] animate-bounce"
                          style={{ animationDelay: '300ms' }}
                        />
                      </span>
                    </div>
                  </div>
                )}

              {isCancelled && (
                <div className="flex items-center gap-3 py-3">
                  <div
                    className="h-px flex-1"
                    style={{
                      background:
                        'linear-gradient(to right, transparent, var(--border-primary), transparent)',
                    }}
                  />
                  <span className="text-xs text-[var(--text-tertiary)] italic">已停止响应</span>
                  <div
                    className="h-px flex-1"
                    style={{
                      background:
                        'linear-gradient(to right, transparent, var(--border-primary), transparent)',
                    }}
                  />
                </div>
              )}

              {approvalRequest && (
                <div className="py-2">
                  <ApprovalCard
                    request={approvalRequest}
                    onApprove={() => sendApproval(approvalRequest.requestId, true)}
                    onReject={(reason) => sendApproval(approvalRequest.requestId, false, reason)}
                  />
                </div>
              )}

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

      {connectionStatus === 'disconnected' && (
        <div
          role="status"
          aria-live="assertive"
          className="flex items-center justify-center gap-2 px-4 py-1.5 text-xs font-medium"
          style={{
            background: 'color-mix(in srgb, var(--accent) 10%, transparent)',
            color: 'var(--accent)',
          }}
        >
          <span
            className="inline-block h-2 w-2 rounded-full"
            style={{ background: 'var(--accent)' }}
          />
          已断开 — 重新连接中...
        </div>
      )}

      <div>
        {isStreaming && messages.length > 0 && (
          <div className="flex justify-center pb-2">
            <button
              onClick={cancel}
              aria-label="停止生成"
              className="flex items-center gap-2 rounded-full border border-[var(--border-primary)] bg-[var(--bg-primary)] px-4 py-2 text-sm font-medium text-[var(--text-secondary)] shadow-[var(--shadow-sm)] transition-all hover:text-[var(--text-primary)]"
            >
              <div
                className="h-3 w-3 rounded-[3px]"
                style={{ background: 'var(--text-secondary)' }}
              />
              停止生成
            </button>
          </div>
        )}
        <ChatInput onSend={handleSend} isStreaming={isStreaming} onCancel={cancel} />
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
    <div className="animate-pulse-border rounded-xl border-2 bg-[var(--bg-primary)] p-4 shadow-[var(--shadow-sm)]">
      {prompt && <p className="mb-2 text-sm font-medium text-[var(--text-primary)]">{prompt}</p>}
      <form onSubmit={handleSubmit} className="flex gap-2">
        <input name="input" className="input flex-1" placeholder="输入你的回答..." autoFocus />
        <button type="submit" className="btn btn-primary">
          提交
        </button>
      </form>
    </div>
  );
}
