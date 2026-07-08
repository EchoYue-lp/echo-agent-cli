import { useRef, useEffect, useCallback, useState } from 'react';
import { useChatStore } from '../../stores/chatStore';
import { MessageBubble } from './MessageBubble';
import { ApprovalCard } from './ApprovalCard';
import { ChatInput } from './ChatInput';
import { WelcomeScreen } from './WelcomeScreen';
import { useTauriChat } from '../../hooks/useTauriChat';
import { useWorkspaceStore } from '../../stores/workspaceStore';
import { useConversationStore } from '../../stores/conversationStore';
import { useSubagentRunStore } from '../../stores/subagentRunStore';
import { useSubagentDetailStore } from '../../stores/subagentDetailStore';
import { useTaskRuntimeStore } from '../../stores/taskRuntimeStore';
import { SubagentDetailView } from '../task/SubagentDetailView';
import { FailureToast } from './FailureToast';
import type { Attachment } from '../../types/api';

// Tauri IPC is the only live transport. The WebSocket transport
// (hooks/useWebSocket.ts) was removed after the chat path migrated to Tauri
// commands (src/tauri/commands/chat.rs).
function useChatTransport() {
  return useTauriChat();
}

export function ChatPanel() {
  const messages = useChatStore((s) => s.messages);
  const approvalRequest = useChatStore((s) => s.approvalRequest);
  const inputRequest = useChatStore((s) => s.inputRequest);
  const selectionRequest = useChatStore((s) => s.selectionRequest);
  const isStreaming = useChatStore((s) => s.isStreaming);
  const isCancelled = useChatStore((s) => s.isCancelled);
  const runStatus = useChatStore((s) => s.runStatus);
  const currentWorkspace = useWorkspaceStore((s) => s.current);
  const subagentRuns = useSubagentRunStore((s) => s.runs);
  const selectedSubagentRef = useSubagentDetailStore((s) => s.selected);
  const closeSubagentDetail = useSubagentDetailStore((s) => s.close);
  const selectedSubagent = selectedSubagentRef
    ? subagentRuns[selectedSubagentRef.subagentRunId]
    : undefined;
  const selectedRunSubagents = selectedSubagent
    ? Object.values(subagentRuns).filter((run) => run.runId === selectedSubagent.runId)
    : [];

  // ── 按需卡片状态 ──
  const [failureToastDismissed, setFailureToastDismissed] = useState(false);

  // Reset failure toast dismiss when a new run starts (status changes)
  useEffect(() => {
    setFailureToastDismissed(false);
  }, [runStatus]);
  const scrollRef = useRef<HTMLDivElement>(null);
  const bottomRef = useRef<HTMLDivElement>(null);
  const isNearBottomRef = useRef(true);
  const { sendMessage, sendApproval, sendInput, sendSelection, cancel } = useChatTransport();

  const clearCurrentChat = useCallback(async () => {
    cancel();
    await useConversationStore.getState().clearCurrent();
    useTaskRuntimeStore.getState().reset();
    useSubagentRunStore.getState().clear();
    closeSubagentDetail();
  }, [cancel, closeSubagentDetail]);

  const handleRegenerate = () => {
    // Cancel any in-flight run before regenerating — otherwise late-arriving
    // token events from the old run land on a deleted message (orphan).
    cancel();
    const store = useChatStore.getState();
    const content = store.prepareRegenerate();
    if (content) sendMessage(content);
  };

  const handleEditAndResend = (messageId: string, newContent: string) => {
    cancel();
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
  }, [messages, approvalRequest, inputRequest, selectionRequest, isStreaming]);

  const handleSuggestionClick = (text: string) => {
    sendMessage(text);
  };

  const handleSend = (text: string, attachments?: Attachment[]) => {
    const command = text.trim().toLowerCase();
    if (!attachments?.length && (command === '/clear' || command === '/cls')) {
      void clearCurrentChat();
      return;
    }
    sendMessage(text, attachments);
  };

  return (
    <div className="flex h-full flex-col min-h-0" role="main" aria-label="聊天面板">
      <div className="flex h-12 shrink-0 items-center justify-between border-b border-[var(--border-primary)] bg-[var(--bg-primary)] px-12">
        <div className="min-w-0">
          <div className="truncate text-sm font-semibold text-[var(--text-primary)]">
            {currentWorkspace?.name || 'EKO'}
          </div>
          <div className="truncate text-[11px] text-[var(--text-tertiary)]">
            {currentWorkspace?.root || '选择或创建一个任务开始工作'}
          </div>
        </div>
        <div className="hidden items-center gap-2 text-xs text-[var(--text-tertiary)] sm:flex">
          <span className="h-2 w-2 rounded-full bg-[var(--accent)]" />
          <span>{runStatusLabel(runStatus, isStreaming)}</span>
        </div>
      </div>
      {selectedSubagent ? (
        <div className="min-h-0 flex-1">
          <SubagentDetailView
            run={selectedSubagent}
            allRuns={selectedRunSubagents}
            onBack={closeSubagentDetail}
          />
        </div>
      ) : (
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
            <div className="mx-auto w-full max-w-[768px] px-5 sm:px-8">
              <div className="space-y-1 pb-4 pt-4">
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
                          {runStatusLabel(runStatus, true)}
                        </span>
                        <span className="flex gap-0.5">
                          <span
                            className="h-1 w-1 rounded-full bg-[var(--accent)] animate-bounce"
                            style={{ animationDelay: '0ms' }}
                          />
                          <span
                            className="h-1 w-1 rounded-full bg-[var(--accent)] animate-bounce"
                            style={{ animationDelay: '150ms' }}
                          />
                          <span
                            className="h-1 w-1 rounded-full bg-[var(--accent)] animate-bounce"
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

                {inputRequest && (
                  <div className="py-2">
                    <InputCard
                      prompt={inputRequest.prompt}
                      onSubmit={(text) => sendInput(inputRequest.requestId, text)}
                    />
                  </div>
                )}

                {selectionRequest && (
                  <div className="py-2">
                    <SelectionCard
                      prompt={selectionRequest.prompt}
                      options={selectionRequest.options}
                      taskId={selectionRequest.taskId}
                      phase={selectionRequest.phase}
                      context={selectionRequest.context}
                      onSelect={(selection, instructions) =>
                        sendSelection(selectionRequest.requestId, selection, instructions)
                      }
                    />
                  </div>
                )}

                {/* Failure toast (spec §3.4) */}
                {!failureToastDismissed && (
                  <div className="py-1">
                    <FailureToast onDismiss={() => setFailureToastDismissed(true)} />
                  </div>
                )}
              </div>
              <div ref={bottomRef} className="h-1" />
            </div>
          )}
        </div>
      )}

      <div>
        {approvalRequest && (
          <div className="mx-auto w-full max-w-[768px] px-5 pb-2 sm:px-8">
            <ApprovalCard
              request={approvalRequest}
              onApprove={() => sendApproval(approvalRequest.requestId, true)}
              onReject={(reason) => sendApproval(approvalRequest.requestId, false, reason)}
              onModify={(feedback) =>
                sendApproval(approvalRequest.requestId, false, `修改意见: ${feedback}`)
              }
              onApproveAll={() =>
                sendApproval(approvalRequest.requestId, true, undefined, 'session_all_tools')
              }
            />
          </div>
        )}
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

function runStatusLabel(status: string, isStreaming: boolean) {
  switch (status) {
    case 'thinking':
      return '思考中';
    case 'using_tool':
      return '调用工具中';
    case 'waiting_approval':
      return '等待审批';
    case 'waiting_input':
      return '等待输入';
    case 'failed':
      return '执行失败';
    case 'cancelled':
      return '已停止';
    case 'running':
      return '执行中';
    default:
      return isStreaming ? '执行中' : '就绪';
  }
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

function SelectionCard({
  prompt,
  options,
  taskId,
  phase,
  context,
  onSelect,
}: {
  prompt: string;
  options: string[];
  taskId?: string;
  phase?: string;
  context?: unknown;
  onSelect: (selection: string, instructions?: string) => void;
}) {
  const instructionsRef = useRef<HTMLTextAreaElement>(null);

  const submitSelection = (selection: string) => {
    const instructions = instructionsRef.current?.value.trim() || undefined;
    onSelect(selection, instructions);
  };

  return (
    <div className="rounded-xl border-2 border-[var(--accent)]/40 bg-[var(--bg-primary)] p-4 shadow-[var(--shadow-sm)]">
      <div className="space-y-3">
        <div>
          <p className="text-sm font-semibold text-[var(--text-primary)]">需要选择</p>
          <p className="mt-1 text-sm text-[var(--text-secondary)]">{prompt}</p>
          {(taskId || phase) && (
            <div className="mt-2 flex flex-wrap gap-2 text-[11px] text-[var(--text-tertiary)]">
              {taskId && <span>任务: {taskId}</span>}
              {phase && <span>阶段: {phase}</span>}
            </div>
          )}
        </div>

        {context !== undefined && (
          <pre className="max-h-36 overflow-auto rounded-lg bg-[var(--bg-code)] p-3 text-xs text-[var(--color-code-text)]">
            {JSON.stringify(context, null, 2)}
          </pre>
        )}

        <textarea
          ref={instructionsRef}
          className="min-h-16 w-full rounded-lg border border-[var(--border-primary)] bg-[var(--bg-input)] px-3 py-2 text-sm text-[var(--text-primary)] outline-none focus:border-[var(--border-focus)]"
          placeholder="补充说明（可选）"
        />

        <div className="flex flex-wrap gap-2">
          {options.length > 0 ? (
            options.map((option) => (
              <button
                key={option}
                type="button"
                onClick={() => submitSelection(option)}
                className="rounded-lg bg-[var(--accent)] px-4 py-2 text-sm font-medium text-[var(--text-on-accent)] transition-opacity hover:opacity-90"
              >
                {option}
              </button>
            ))
          ) : (
            <button
              type="button"
              onClick={() => submitSelection('approve')}
              className="rounded-lg bg-[var(--accent)] px-4 py-2 text-sm font-medium text-[var(--text-on-accent)] transition-opacity hover:opacity-90"
            >
              继续
            </button>
          )}
        </div>
      </div>
    </div>
  );
}
