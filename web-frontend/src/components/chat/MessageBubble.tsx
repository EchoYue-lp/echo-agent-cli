import { AgentInputMessage } from './AgentInputMessage';
import { useMemo, useState, memo } from 'react';
import { transcriptToolExecution, type ChatMessage } from '../../types/api';
import { Copy, Check, RefreshCw, GitFork, Pencil, X, ArrowUp, File, Download } from 'lucide-react';
import MarkdownContent from '../common/MarkdownContent';
import { NativeMessageTimeline, nativeAssistantBlockText } from './NativeMessageTimeline';
import { ParallelExecutionBlock, visibleSubagentRuns } from './ParallelExecutionBlock';
import { ToolExecutionGroup } from './ToolExecutionGroup';
import { ExecutionProcessGroup } from './ExecutionProcessGroup';
import { useSubagentRunStore } from '../../stores/subagentRunStore';
import { useTaskRuntimeStore } from '../../stores/taskRuntimeStore';
import { useChatStore } from '../../stores/chatStore';
import { useToolExecutionStore } from '../../stores/toolExecutionStore';

interface MessageBubbleProps {
  message: ChatMessage;
  onRegenerate?: (messageId: string) => void;
  onEditAndResend?: (messageId: string, newContent: string) => void;
  onFork?: (messageId: string) => void;
  forkDisabled?: boolean;
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
  } catch {
    // Fall through to the legacy clipboard path.
  }
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

/** 原生Reasoning由NativeMessageTimeline呈现；此投影只处理当前工具引用。 */
interface FlatStep {
  type: 'tool';
  toolId: string;
  toolIndex: number;
}

interface ToolDisplayItem {
  type: 'tools';
  toolIds: string[];
}

type ExecutionDisplayItem = ToolDisplayItem;

export function isExecutionProcessCompleted(
  messageIsStreaming: boolean | undefined,
  activeRunBelongsToMessage: boolean,
  activeRunStatus: string | null,
  subagentStatuses: readonly string[],
  hasRunningTools = false
): boolean {
  if (messageIsStreaming) return false;
  if (activeRunBelongsToMessage) {
    // TaskRun is the authoritative lifecycle. A missing or delayed Subagent
    // or tool terminal event must not leave a completed task expanded forever.
    return Boolean(
      activeRunStatus && ['completed', 'failed', 'cancelled'].includes(activeRunStatus)
    );
  }
  return !hasRunningTools && subagentStatuses.every((status) => status !== 'running');
}

export function flattenSteps(message: ChatMessage): { steps: FlatStep[] } {
  const steps: FlatStep[] = [];
  for (const step of message.executionSteps ?? []) {
    if (step.type === 'tool')
      steps.push({ type: 'tool', toolId: step.callId, toolIndex: steps.length });
  }
  return { steps };
}

/** 无native块的工具视图仍沿canonical步骤顺序呈现。 */
export function groupExecutionSteps(steps: FlatStep[]): ExecutionDisplayItem[] {
  return steps.length ? [{ type: 'tools', toolIds: steps.map((step) => step.toolId) }] : [];
}

/** Read only the tool facts already proved for each displayed reference. */
function displayedToolStatuses(
  message: ChatMessage,
  items: ExecutionDisplayItem[],
  tools: ReturnType<typeof useToolExecutionStore.getState>['tools']
): (string | undefined)[] {
  const ids = new Set(items.flatMap((item) => (item.type === 'tools' ? item.toolIds : [])));
  return [...ids].map((id) => {
    const record = message.transcriptTools?.find((entry) => entry.id === id);
    return (record ? transcriptToolExecution(record, tools) : tools[id])?.status;
  });
}

export const MessageBubble = memo(function MessageBubble({
  message,
  onRegenerate,
  onEditAndResend,
  onFork,
  forkDisabled,
}: MessageBubbleProps) {
  const isUser = message.role === 'user' && !message.internalAgent;
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

  const { steps } = flattenSteps(message);
  const executionItems = groupExecutionSteps(steps);
  const hasNativeTimeline = Boolean(message.assistantBlocks?.length);
  const hasVisibleNativeContent = message.assistantBlocks?.some(
    (view) => view.block.phase !== 'final_answer' && Boolean(nativeAssistantBlockText(view).trim())
  );
  const activeRun = useTaskRuntimeStore((state) => state.activeRun);
  const subagentRuns = useSubagentRunStore((state) => state.runs);
  const chatMessages = useChatStore((state) => state.messages);
  const lastAssistantMessageId = useMemo(() => {
    for (let index = chatMessages.length - 1; index >= 0; index -= 1) {
      const candidate = chatMessages[index];
      if (candidate?.role === 'assistant') return candidate.id;
    }
    return null;
  }, [chatMessages]);
  const messageIds = useMemo(() => new Set(chatMessages.map((item) => item.id)), [chatMessages]);
  const visibleRuns = useMemo(
    () =>
      message.role === 'tool'
        ? []
        : visibleSubagentRuns(
            Object.values(subagentRuns),
            activeRun,
            message.id,
            lastAssistantMessageId,
            messageIds
          ),
    [activeRun, lastAssistantMessageId, message.id, message.role, messageIds, subagentRuns]
  );
  const activeRunBelongsToMessage = Boolean(
    activeRun &&
    (activeRun.root_message_id === message.id ||
      (!messageIds.has(activeRun.root_message_id) && message.id === lastAssistantMessageId))
  );
  const hasRunningTools = useToolExecutionStore((state) =>
    displayedToolStatuses(message, executionItems, state.tools).includes('running')
  );
  const toolFailureCount = useToolExecutionStore(
    (state) =>
      displayedToolStatuses(message, executionItems, state.tools).filter(
        (status) => status === 'failed' || status === 'timed_out'
      ).length
  );
  // Keep the existing action/turn lifecycle gate. Tool-summary knowledge only
  // refines the process presentation and does not create another Run state.
  const executionCompleted = isExecutionProcessCompleted(
    message.isStreaming,
    activeRunBelongsToMessage,
    activeRun?.status ?? null,
    visibleRuns.map((run) => run.status)
  );
  const processCompleted = isExecutionProcessCompleted(
    message.isStreaming,
    activeRunBelongsToMessage,
    activeRun?.status ?? null,
    visibleRuns.map((run) => run.status),
    hasRunningTools
  );
  const hasExecutionProcess = executionItems.length > 0 || visibleRuns.length > 0;
  const hasUnrestoredTranscript = useToolExecutionStore(
    (state) =>
      (message.transcriptTools?.some((record) => !transcriptToolExecution(record, state.tools)) ??
        false) ||
      displayedToolStatuses(message, executionItems, state.tools).some(
        (status) => status == null || status === 'unknown' || status === 'interrupted'
      )
  );
  const hasBody = Boolean(message.content.trim());

  if (message.role === 'user' && message.agentInput) return <AgentInputMessage message={message} />;

  // A canonical tool record is not an assistant turn and cannot own Subagents,
  // regeneration or Fork. Its key addresses this display row only.
  if (message.role === 'tool') {
    return (
      <div className="animate-fade-up py-3">
        {message.internalAgent && (
          <span className="text-[10px] text-[var(--text-tertiary)]">Agent 内部工具记录</span>
        )}
        <ToolExecutionGroup
          toolIds={message.transcriptTools?.map((record) => record.id) ?? []}
          transcriptTools={message.transcriptTools}
        />
        {message.content && (
          <ActionButton
            icon={<Copy size={13} />}
            label="复制原记录"
            onClick={() => copyToClipboard(message.content)}
            copyMode
          />
        )}
      </div>
    );
  }

  // Canonical history includes empty assistant rows for tool calls. Render only
  // their visible projections; ChatPanel owns the single empty-stream status.
  if (
    !isUser &&
    !hasBody &&
    !hasVisibleNativeContent &&
    images.length === 0 &&
    files.length === 0 &&
    !hasExecutionProcess &&
    !message.internalAgent
  ) {
    return null;
  }

  return (
    <div className={`animate-fade-up py-3 ${isUser ? 'ml-auto w-fit max-w-[85%]' : 'w-full'}`}>
      {/* Content */}
      <div className="min-w-0 space-y-2">
        {/* Images */}
        {images.length > 0 && (
          <div className={`grid gap-2 ${images.length === 1 ? 'grid-cols-1' : 'grid-cols-2'}`}>
            {images.map((img, i) => (
              <div
                key={i}
                className="overflow-hidden rounded-lg border border-[var(--border-primary)] bg-[var(--bg-secondary)]"
              >
                <button
                  type="button"
                  aria-label={`打开图片 ${img.name}`}
                  onClick={() => window.open(img.url, '_blank')}
                  className="block w-full border-0 bg-transparent p-0"
                >
                  <img
                    src={img.url}
                    alt={img.name}
                    className="w-full object-cover"
                    style={{ maxHeight: '300px' }}
                  />
                </button>
              </div>
            ))}
          </div>
        )}

        {/* Files */}
        {files.length > 0 && (
          <div className="space-y-1.5">
            {files.map((file, i) => {
              const body = (
                <>
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
                  {file.url && (
                    <Download size={14} className="shrink-0 text-[var(--text-tertiary)]" />
                  )}
                </>
              );
              const className =
                'flex items-center gap-3 rounded-lg border border-[var(--border-primary)] bg-[var(--bg-secondary)] p-3 transition-colors hover:bg-[var(--bg-hover)]';
              return file.url ? (
                <a key={i} href={file.url} download={file.name} className={className}>
                  {body}
                </a>
              ) : (
                <div key={i} className={className}>
                  {body}
                </div>
              );
            })}
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
              <div className="rounded-lg bg-[var(--bg-user-bubble)] px-3 py-2.5 text-sm leading-relaxed text-[var(--text-primary)]">
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
            {message.internalAgent && (
              <span className="inline-flex h-5 items-center rounded border border-[var(--border-primary)] px-1.5 text-[10px] font-medium text-[var(--text-tertiary)]">
                Agent 内部消息
              </span>
            )}
            {hasNativeTimeline && <NativeMessageTimeline message={message} />}
            {!hasNativeTimeline && hasExecutionProcess ? (
              <ExecutionProcessGroup
                completed={processCompleted}
                unrestored={hasUnrestoredTranscript}
                toolFailureCount={toolFailureCount}
              >
                {/* canonical工具引用 */}
                <div className="space-y-1">
                  {executionItems.map((item, i) => {
                    return (
                      <ToolExecutionGroup
                        key={`tools-${i}`}
                        toolIds={item.toolIds}
                        transcriptTools={message.transcriptTools}
                      />
                    );
                  })}
                </div>

                {/* Run-level Subagents remain independent rows inside the process. */}
                <ParallelExecutionBlock messageId={message.id} />
              </ExecutionProcessGroup>
            ) : (
              // No execution history exists for this assistant response.
              <ParallelExecutionBlock messageId={message.id} />
            )}

            {/* Final text — no left border, plain markdown flow */}
            {hasBody && (
              <div className="group/msg relative">
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
                {!message.isStreaming && !editing && (
                  <div className="mt-2 flex items-center gap-0.5 text-[var(--text-tertiary)]">
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
                        onClick={() => onRegenerate(message.id)}
                      />
                    )}
                    {onFork && !message.internalAgent && message.finalAnswerConfirmed === true && (
                      <ActionButton
                        icon={<GitFork size={13} />}
                        label="分叉会话"
                        onClick={() => onFork(message.id)}
                        disabled={forkDisabled || !executionCompleted}
                      />
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
  disabled = false,
}: {
  icon: React.ReactNode;
  label: string;
  onClick: () => void;
  copyMode?: boolean;
  disabled?: boolean;
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
      disabled={disabled}
      aria-label={label}
      className={`flex items-center gap-1 rounded-md px-1.5 py-1 text-[11px] transition-colors disabled:cursor-not-allowed disabled:opacity-40 ${done ? 'text-[var(--color-success)]' : 'text-[var(--text-tertiary)] hover:text-[var(--text-primary)]'}`}
      title={label}
    >
      {done ? <Check size={13} /> : icon}
    </button>
  );
}
