import { useRef, useEffect, useCallback, useState } from 'react';
import { useChatStore } from '../../stores/chatStore';
import { MessageBubble } from './MessageBubble';
import { ApprovalCard } from './ApprovalCard';
import { ChatInput } from './ChatInput';
import { WelcomeScreen } from './WelcomeScreen';
import { useTauriChat } from '../../hooks/useTauriChat';
import { useWorkspaceStore } from '../../stores/workspaceStore';
import { useConversationStore } from '../../stores/conversationStore';
import { useToastStore } from '../../stores/toastStore';
import { subagentRunStoreKey, useSubagentRunStore } from '../../stores/subagentRunStore';
import { useSubagentDetailStore } from '../../stores/subagentDetailStore';
import { useTaskRuntimeStore } from '../../stores/taskRuntimeStore';
import { SubagentDetailView } from '../task/SubagentDetailView';
import { FailureToast } from './FailureToast';
import { CornerUpLeft, GripVertical, MessagesSquare, PanelRightOpen, X } from 'lucide-react';
import { AgentMessageDialog } from './AgentMessageDialog';
import type { Attachment } from '../../types/api';
import type { QueuedChatInput } from '../../hooks/useTauriChat';
import { useRightWorkspaceStore } from '../../stores/rightWorkspaceStore';
import { useToolExecutionStore } from '../../stores/toolExecutionStore';
import { dispatchGuiSlashCommand } from '../../lib/slashCommands';

// Tauri IPC is the only live transport. The WebSocket transport
// (hooks/useWebSocket.ts) was removed after the chat path migrated to Tauri
// commands (src/tauri/commands/chat.rs).
function useChatTransport() {
  return useTauriChat();
}

export function ChatPanel() {
  const messages = useChatStore((s) => s.messages);
  const pendingHitlRequest = useChatStore((s) => s.pendingHitlRequests.at(0) ?? null);
  const isStreaming = useChatStore((s) => s.isStreaming);
  const isCancelled = useChatStore((s) => s.isCancelled);
  const runStatus = useChatStore((s) => s.runStatus);
  const currentWorkspace = useWorkspaceStore((s) => s.current);
  const rightWorkspace = useRightWorkspaceStore();
  const todoCount = useTaskRuntimeStore((state) => state.todos.length);
  const subagentRuns = useSubagentRunStore((s) => s.runs);
  const selectedSubagentRef = useSubagentDetailStore((s) => s.selected);
  const closeSubagentDetail = useSubagentDetailStore((s) => s.close);
  const selectedSubagent = selectedSubagentRef
    ? subagentRuns[
        subagentRunStoreKey(selectedSubagentRef.runId, selectedSubagentRef.subagentRunId)
      ]
    : undefined;

  // ── 按需卡片状态 ──
  const [failureToastDismissed, setFailureToastDismissed] = useState(false);
  const [agentMessagesOpen, setAgentMessagesOpen] = useState(false);

  // Reset failure toast dismiss when a new run starts (status changes)
  useEffect(() => {
    setFailureToastDismissed(false);
  }, [runStatus]);
  const scrollRef = useRef<HTMLDivElement>(null);
  const bottomRef = useRef<HTMLDivElement>(null);
  const isNearBottomRef = useRef(true);
  const {
    sendMessage,
    sendApproval,
    sendInput,
    sendSelection,
    cancel,
    queuedInputs,
    clearQueuedMessages,
    removeQueuedMessage,
    reorderQueuedMessage,
    steerQueuedMessage,
  } = useChatTransport();

  const clearCurrentChat = useCallback(async () => {
    clearQueuedMessages();
    cancel();
    await useConversationStore.getState().clearCurrent();
    useTaskRuntimeStore.getState().reset();
    useSubagentRunStore.getState().clear();
    closeSubagentDetail();
  }, [cancel, clearQueuedMessages, closeSubagentDetail]);

  const branchAndResend = async (messageId: string, newContent?: string) => {
    clearQueuedMessages();
    await cancel();
    const chatStore = useChatStore.getState();
    const messageIndex = chatStore.messages.findIndex((message) => message.id === messageId);
    if (messageIndex < 0) return;
    const userMessageIndex = newContent
      ? messageIndex
      : chatStore.messages
          .slice(0, messageIndex)
          .findLastIndex((message) => message.role === 'user');
    if (userMessageIndex < 0 || chatStore.messages[userMessageIndex]?.role !== 'user') return;
    const userTurnIndex = chatStore.messages
      .slice(0, userMessageIndex)
      .filter((message) => message.role === 'user').length;
    const userMessage = chatStore.messages[userMessageIndex];
    if (!userMessage) return;
    const content = newContent ?? userMessage.content;
    const attachments = userMessage.attachments;
    const hasUnavailableAttachment = attachments?.some(
      (attachment) => !attachment.url.includes(';base64,')
    );
    if (hasUnavailableAttachment) {
      useToastStore.getState().addToast('error', '该历史消息的附件已转为文件引用，无法自动重建');
      return;
    }
    try {
      const branch = await useConversationStore.getState().branchCurrent(userTurnIndex);
      const prefix = chatStore.messages.slice(0, userMessageIndex).map((message, index) => ({
        ...message,
        id: `loaded-${branch.id}-${index}`,
        executionSteps: undefined,
        executionRounds: undefined,
      }));
      chatStore.replaceMessages(prefix);
      useToolExecutionStore.getState().clear();
      useTaskRuntimeStore.getState().reset();
      useSubagentRunStore.getState().clear();
      const resendAttachments = attachments?.flatMap((attachment) => {
        const marker = ';base64,';
        const markerIndex = attachment.url.indexOf(marker);
        if (markerIndex < 0) return [];
        return [
          {
            name: attachment.name,
            mime_type: attachment.mime_type,
            data: attachment.url.slice(markerIndex + marker.length),
            size: attachment.size,
            source: attachment.source,
          } satisfies Attachment,
        ];
      });
      await sendMessage(newContent ? content : branch.targetContent, resendAttachments);
    } catch (error) {
      console.error('Failed to branch conversation:', error);
      useToastStore.getState().addToast('error', '创建会话分支失败，请重试');
    }
  };

  const handleRegenerate = (messageId: string) => {
    void branchAndResend(messageId);
  };

  const handleEditAndResend = (messageId: string, newContent: string) => {
    void branchAndResend(messageId, newContent);
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
  }, [messages, pendingHitlRequest, isStreaming]);

  const handleSuggestionClick = (text: string) => {
    sendMessage(text);
  };

  const handleSend = async (text: string, attachments?: Attachment[]) => {
    if (!attachments?.length) {
      const dispatched = await dispatchGuiSlashCommand(text, {
        clear: clearCurrentChat,
        tasks: rightWorkspace.openTasks,
        analysis: rightWorkspace.openAnalysis,
        research: rightWorkspace.openResearch,
        browser: rightWorkspace.openBrowser,
        files: rightWorkspace.openFiles,
        workflows: rightWorkspace.openWorkflows,
        extract: rightWorkspace.openExtract,
      });
      if (dispatched) return true;
    }
    return sendMessage(text, attachments);
  };

  return (
    <div
      className="flex h-full min-h-0 flex-col bg-[var(--bg-chat)]"
      role="main"
      aria-label="聊天面板"
    >
      <div className="flex h-11 shrink-0 items-center justify-between border-b border-[var(--border-secondary)] bg-[var(--bg-chat)]/95 px-12 backdrop-blur">
        <div className="min-w-0">
          <div className="truncate text-[13px] font-medium text-[var(--text-primary)]">
            {currentWorkspace?.name || 'EKO'}
          </div>
          <div className="truncate text-[10px] text-[var(--text-tertiary)]">
            {currentWorkspace?.root || '选择或创建一个任务开始工作'}
          </div>
        </div>
        <div className="flex items-center gap-2 text-xs text-[var(--text-tertiary)]">
          <div className="hidden items-center gap-2 sm:flex">
            <span className="h-2 w-2 rounded-full bg-[var(--accent)]" />
            <span>{runStatusLabel(runStatus, isStreaming)}</span>
          </div>
          <button
            type="button"
            onClick={() => setAgentMessagesOpen(true)}
            className="flex h-7 w-7 items-center justify-center rounded-md text-[var(--text-tertiary)] transition-colors hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]"
            title="Agent 消息"
            aria-label="打开 Agent 消息"
          >
            <MessagesSquare size={15} />
          </button>
          <button
            type="button"
            onClick={rightWorkspace.openWorkspace}
            className={`relative flex h-7 w-7 items-center justify-center rounded-md transition-colors hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)] ${rightWorkspace.open ? 'bg-[var(--bg-hover)] text-[var(--text-primary)]' : 'text-[var(--text-tertiary)]'}`}
            title="右侧工作区"
            aria-label="打开右侧工作区"
          >
            <PanelRightOpen size={15} />
            {todoCount > 0 && (
              <span className="absolute -right-0.5 -top-0.5 min-w-3 rounded-full bg-[var(--accent)] px-0.5 text-center text-[8px] leading-3 text-white">
                {todoCount > 9 ? '9+' : todoCount}
              </span>
            )}
          </button>
        </div>
      </div>
      {selectedSubagent ? (
        <div className="min-h-0 flex-1">
          <SubagentDetailView run={selectedSubagent} onBack={closeSubagentDetail} />
        </div>
      ) : (
        <div
          ref={scrollRef}
          className="min-h-0 flex-1 overflow-y-auto"
          onScroll={handleScroll}
          role="log"
          aria-live="polite"
          aria-label="消息列表"
        >
          {messages.length === 0 ? (
            <WelcomeScreen onSuggestionClick={handleSuggestionClick} />
          ) : (
            <div className="mx-auto w-full max-w-[980px] px-4 sm:px-6 lg:px-8">
              <div className="space-y-1 pb-6 pt-5">
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

                {pendingHitlRequest?.kind === 'input' && (
                  <div className="py-2">
                    <InputCard
                      prompt={pendingHitlRequest.prompt}
                      onSubmit={(text) => sendInput(pendingHitlRequest.requestId, text)}
                    />
                  </div>
                )}

                {pendingHitlRequest?.kind === 'selection' && (
                  <div className="py-2">
                    <SelectionCard
                      prompt={pendingHitlRequest.prompt}
                      options={pendingHitlRequest.options}
                      taskId={pendingHitlRequest.taskId}
                      phase={pendingHitlRequest.phase}
                      context={pendingHitlRequest.context}
                      onSelect={(selection, instructions) =>
                        sendSelection(pendingHitlRequest.requestId, selection, instructions)
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

      <div className="shrink-0 bg-[linear-gradient(to_top,var(--bg-chat)_72%,transparent)]">
        {pendingHitlRequest?.kind === 'approval' && (
          <div className="mx-auto w-full max-w-[980px] px-4 pb-2 sm:px-6 lg:px-8">
            <ApprovalCard
              request={pendingHitlRequest}
              onApprove={() => sendApproval(pendingHitlRequest.requestId, true)}
              onReject={(reason) => sendApproval(pendingHitlRequest.requestId, false, reason)}
              onModify={(feedback) =>
                sendApproval(pendingHitlRequest.requestId, false, `修改意见: ${feedback}`)
              }
              onApproveAll={() =>
                sendApproval(pendingHitlRequest.requestId, true, undefined, 'session_tool')
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
        {queuedInputs.length > 0 && (
          <QueuedInputList
            items={queuedInputs}
            onRemove={removeQueuedMessage}
            onReorder={reorderQueuedMessage}
            onSteer={steerQueuedMessage}
          />
        )}
        <ChatInput
          onSend={handleSend}
          isStreaming={isStreaming}
          onCancel={cancel}
          queuedCount={queuedInputs.length}
        />
      </div>
      <AgentMessageDialog isOpen={agentMessagesOpen} onClose={() => setAgentMessagesOpen(false)} />
    </div>
  );
}

function QueuedInputList({
  items,
  onRemove,
  onReorder,
  onSteer,
}: {
  items: QueuedChatInput[];
  onRemove: (id: string) => void;
  onReorder: (sourceId: string, targetId: string) => void;
  onSteer: (id: string) => Promise<boolean>;
}) {
  const [draggedId, setDraggedId] = useState<string | null>(null);

  return (
    <section className="mx-auto mb-2 w-full max-w-[980px] px-4 sm:px-6 lg:px-8">
      <div className="mb-1 flex items-center justify-between px-1 text-[11px] text-[var(--text-tertiary)]">
        <span>排队任务 {items.length}</span>
        <span>拖动调整执行顺序</span>
      </div>
      <div className="max-h-40 space-y-1 overflow-y-auto">
        {items.map((item, index) => (
          <div
            key={item.id}
            draggable
            onDragStart={() => setDraggedId(item.id)}
            onDragEnd={() => setDraggedId(null)}
            onDragOver={(event) => event.preventDefault()}
            onDrop={(event) => {
              event.preventDefault();
              if (draggedId) onReorder(draggedId, item.id);
              setDraggedId(null);
            }}
            className="flex min-h-9 items-center gap-2 border-l-2 border-[var(--border-primary)] bg-[var(--bg-secondary)] px-2 py-1.5 text-xs transition-colors hover:bg-[var(--bg-hover)]"
            style={{ opacity: draggedId === item.id ? 0.55 : 1 }}
          >
            <GripVertical
              size={14}
              className="shrink-0 cursor-grab text-[var(--text-tertiary)] active:cursor-grabbing"
              aria-hidden="true"
            />
            <span className="w-5 shrink-0 text-center tabular-nums text-[var(--text-tertiary)]">
              {index + 1}
            </span>
            <span className="min-w-0 flex-1 truncate text-[var(--text-secondary)]">
              {item.text || item.attachments?.map((file) => file.name).join(', ') || '附件'}
            </span>
            <button
              type="button"
              onClick={() => void onSteer(item.id)}
              className="flex h-7 w-7 shrink-0 items-center justify-center text-[var(--text-tertiary)] hover:text-[var(--accent)]"
              title="补充到当前任务"
              aria-label="补充到当前任务"
            >
              <CornerUpLeft size={14} />
            </button>
            <button
              type="button"
              onClick={() => onRemove(item.id)}
              className="flex h-7 w-7 shrink-0 items-center justify-center text-[var(--text-tertiary)] hover:text-[var(--color-error)]"
              title="移出队列"
              aria-label="移出队列"
            >
              <X size={14} />
            </button>
          </div>
        ))}
      </div>
    </section>
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
