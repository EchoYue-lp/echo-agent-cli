import {
  agentInputMessages,
  withAgentInputs,
  type AgentCommunicationInput,
} from '../../lib/agentCommunication';
import { useAgentCommunicationInputs } from '../../hooks/useAgentCommunicationInputs';
import {
  useRef,
  useEffect,
  useCallback,
  useState,
  useImperativeHandle,
  type RefObject,
} from 'react';
import { useChatStore } from '../../stores/chatStore';
import { ConversationTimeline } from './ConversationTimeline';
import { nativeAssistantBlockText } from './NativeMessageTimeline';
import { ReplyForkDialog, type ReplyForkSource } from './ReplyForkDialog';
import { SideConversationDialog } from './SideConversationDialog';
import { Modal } from '../common/Modal';
import { ExecutionStatus } from './ExecutionStatus';
import { HitlDialog } from './HitlDialog';
import { ChatInput } from './ChatInput';
import { WelcomeScreen } from './WelcomeScreen';
import { useTauriChat, type ChatSendSubmission } from '../../hooks/useTauriChat';
import { productDataScope, sameProductDataScope } from '../../lib/productDataScope';
import { configuredModelPresentation, modelLabel } from '../../lib/modelPresentation';
import { useWorkspaceStore } from '../../stores/workspaceStore';
import { useConversationStore } from '../../stores/conversationStore';
import type { SideConversationSource } from '../../stores/conversationStore';
import { useToastStore } from '../../stores/toastStore';
import { useSubagentRunStore } from '../../stores/subagentRunStore';
import { useTaskRuntimeStore } from '../../stores/taskRuntimeStore';
import { FailureToast } from './FailureToast';
import {
  ArrowDown,
  ArrowUp,
  BookOpen,
  Bot,
  CornerUpLeft,
  FileCode,
  FileJson,
  FlaskConical,
  Globe2,
  GripVertical,
  MoreHorizontal,
  PanelRightOpen,
  Workflow,
  X,
} from 'lucide-react';
import { taskRuntimeApi, type ProductDataScope } from '../../api/endpoints';
import type { Attachment, ChatMessage } from '../../types/api';
import type { QueuedChatInput } from '../../hooks/useTauriChat';
import { useContextPaneStore } from '../../stores/contextPaneStore';
import { useWorkspaceViewStore } from '../../stores/workspaceViewStore';
import { useUiStore } from '../../stores/uiStore';
import { useToolExecutionStore } from '../../stores/toolExecutionStore';
import { dispatchGuiSlashCommand, parseGuiSlashCommand } from '../../lib/slashCommands';
import { AgentPane } from './AgentPane';
import { workspaceLocationLabel } from '../../lib/workspacePresentation';
import { CHAT_GUTTER } from '../../lib/layoutMetrics';
import type { ConfiguredModel, ModelProviderView, TaskRun } from '../../generated';

// Tauri IPC is the only live transport. The WebSocket transport
// (hooks/useWebSocket.ts) was removed after the chat path migrated to Tauri
// commands (src/tauri/commands/chat.rs).
function useChatTransport() {
  return useTauriChat();
}

const EMPTY_AGENT_INPUTS: AgentCommunicationInput[] = [];

type GoalCommandSource = Omit<SideConversationSource, 'conversationId'> & {
  conversationId: string | null;
  requestId: string;
  text: string;
};

function ownsCommandContext(source: GoalCommandSource | SideConversationSource) {
  const workspace = useWorkspaceStore.getState();
  const conversation = useConversationStore.getState();
  return (
    !workspace.isLoading &&
    sameProductDataScope(source.scope, productDataScope(workspace.current)) &&
    conversation.newConversationEpoch === source.conversationEpoch &&
    useUiStore.getState().navigationEpoch === source.navigationEpoch
  );
}

function ownsCommandSource(source: GoalCommandSource | SideConversationSource) {
  return (
    ownsCommandContext(source) && useConversationStore.getState().activeId === source.conversationId
  );
}

export function ChatPanel() {
  const messages = useChatStore((s) => s.messages);
  const pendingHitlRequests = useChatStore((s) => s.pendingHitlRequests);
  const pendingHitlRequest = pendingHitlRequests.at(0) ?? null;
  const isStreaming = useChatStore((s) => s.isStreaming);
  const hasActivity = useChatStore((s) => s.activity !== null);
  const isCancelled = useChatStore((s) => s.isCancelled);
  const runStatus = useChatStore((s) => s.runStatus);
  const currentWorkspace = useWorkspaceStore((s) => s.current);
  const contextTarget = useContextPaneStore((state) => state.target);
  const openTasks = useContextPaneStore((state) => state.openTasks);
  const openBrowser = useContextPaneStore((state) => state.openBrowser);
  const openFiles = useContextPaneStore((state) => state.openFiles);
  const openWorkspaceView = useWorkspaceViewStore((state) => state.open);
  const todoCount = useTaskRuntimeStore((state) => state.todos.length);
  const activeTaskRun = useTaskRuntimeStore((state) => state.activeRun);
  const taskScope = useTaskRuntimeStore((state) => state.taskScope);
  const activeConversationId = useConversationStore((state) => state.activeId);
  const conversationEpoch = useConversationStore((state) => state.newConversationEpoch);
  const conversations = useConversationStore((state) => state.conversations);
  const sideConversations = useConversationStore((state) => state.sideConversations);
  const updateSideConversationModel = useConversationStore(
    (state) => state.updateSideConversationModel
  );
  const noteSideConversationTurnStarted = useConversationStore(
    (state) => state.noteSideConversationTurnStarted
  );
  const activeConversation = conversations.find((item) => item.id === activeConversationId);
  // The delivered request keeps its own conversation when the selected page changes.
  const pendingConversationId = pendingHitlRequest?.identity.scope.conversation_id;
  const pendingConversationLabel =
    conversations.find((item) => item.id === pendingConversationId)?.title ??
    pendingConversationId ??
    undefined;
  const activeSide = sideConversations.find(
    (item) => item.conversation_id === activeConversationId
  );
  const activeSideConversationId = activeSide?.conversation_id;

  // ── 按需卡片状态 ──
  const [failureToastDismissed, setFailureToastDismissed] = useState(false);
  const [forkSource, setForkSource] = useState<ReplyForkSource | null>(null);
  const [configuredModels, setConfiguredModels] = useState<ConfiguredModel[]>([]);
  const [modelProviders, setModelProviders] = useState<ModelProviderView[]>([]);
  const [workspaceMenuOpen, setWorkspaceMenuOpen] = useState(false);
  const [goalDraft, setGoalDraft] = useState<GoalCommandSource | null>(null);
  const [sideDraft, setSideDraft] = useState<
    (SideConversationSource & { requestId: string; text: string }) | null
  >(null);
  const goalControls = useRef<GoalBarControls | null>(null);
  const navigationEpoch = useUiStore((state) => state.navigationEpoch);
  const workspaceMenuRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (goalDraft && !ownsCommandSource(goalDraft)) setGoalDraft(null);
    if (sideDraft && !ownsCommandSource(sideDraft)) setSideDraft(null);
  }, [
    goalDraft,
    sideDraft,
    currentWorkspace,
    activeConversationId,
    conversationEpoch,
    navigationEpoch,
  ]);

  // Reset failure toast dismiss when a new run starts (status changes)
  useEffect(() => {
    setFailureToastDismissed(false);
  }, [runStatus]);

  useEffect(() => {
    if (!workspaceMenuOpen) return;
    const closeOnPointerDown = (event: PointerEvent) => {
      const target = event.target;
      if (target instanceof Node && !workspaceMenuRef.current?.contains(target)) {
        setWorkspaceMenuOpen(false);
      }
    };
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key === 'Escape') setWorkspaceMenuOpen(false);
    };
    document.addEventListener('pointerdown', closeOnPointerDown);
    window.addEventListener('keydown', closeOnEscape);
    return () => {
      document.removeEventListener('pointerdown', closeOnPointerDown);
      window.removeEventListener('keydown', closeOnEscape);
    };
  }, [workspaceMenuOpen]);

  useEffect(() => {
    if (!activeSideConversationId) {
      setConfiguredModels([]);
      return;
    }
    let current = true;
    configuredModelPresentation()
      .then((response) => {
        if (current) {
          setConfiguredModels(response.models.filter((model) => model.enabled));
          setModelProviders(response.providers);
        }
      })
      .catch(() => {
        if (current) setConfiguredModels([]);
      });
    return () => {
      current = false;
    };
  }, [activeSideConversationId]);

  const agentInputBinding = useAgentCommunicationInputs(
    useConversationStore((state) => state.workspaceId),
    activeConversationId
  );
  const agentInputs = useChatStore((state) =>
    state.agentInputsBinding === agentInputBinding ? state.agentInputs : EMPTY_AGENT_INPUTS
  );
  const agentInputReadError = useChatStore((state) =>
    state.agentInputReadError?.binding === agentInputBinding
      ? state.agentInputReadError.message
      : null
  );
  const timelineMessages = withAgentInputs(
    messages,
    agentInputMessages(
      agentInputs,
      useConversationStore((state) => state.workspaceId),
      activeConversationId ?? ''
    )
  );

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
    isSubmitting,
    clearQueuedMessages,
    removeQueuedMessage,
    abandonChannelRecoveryInput,
    replaceQueuedMessage,
    reorderQueuedMessage,
    steerQueuedMessage,
  } = useChatTransport();

  const clearCurrentChat = useCallback(async () => {
    clearQueuedMessages();
    cancel();
    await useConversationStore.getState().clearCurrent();
    useTaskRuntimeStore.getState().reset();
    useSubagentRunStore.getState().clear();
    useContextPaneStore.getState().reset();
  }, [cancel, clearQueuedMessages]);

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
          .findLastIndex((message) => message.role === 'user' && !message.internalAgent);
    if (userMessageIndex < 0 || chatStore.messages[userMessageIndex]?.role !== 'user') return;
    const userTurnIndex = chatStore.messages
      .slice(0, userMessageIndex)
      .filter((message) => message.role === 'user' && !message.internalAgent).length;
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

  const forkReplies = completedReplyForks(messages);
  const handleFork = (messageId: string) => {
    const conversation = useConversationStore.getState();
    const workspace = useWorkspaceStore.getState();
    if (
      forkSource ||
      useChatStore.getState().isStreaming ||
      workspace.isLoading ||
      conversation.isLoading ||
      !conversation.activeId
    )
      return;
    const userTurnIndex = completedReplyForks(useChatStore.getState().messages).get(messageId);
    if (userTurnIndex == null) return;
    setForkSource({
      scope: productDataScope(workspace.current),
      conversationId: conversation.activeId,
      conversationEpoch: conversation.newConversationEpoch,
      navigationEpoch: useUiStore.getState().navigationEpoch,
      userTurnIndex,
      requestId: crypto.randomUUID(),
    });
  };

  const handleScroll = useCallback(() => {
    const el = scrollRef.current;
    if (!el) return;
    const nearBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 120;
    isNearBottomRef.current = nearBottom;
  }, []);

  useEffect(() => {
    if (isNearBottomRef.current && bottomRef.current) {
      bottomRef.current.scrollIntoView({
        behavior: isStreaming ? 'auto' : 'smooth',
      });
    }
  }, [messages, agentInputs, pendingHitlRequest, isStreaming]);

  const openPrimaryWorkbench = (view: 'analysis' | 'research' | 'workflow' | 'extract') => {
    useUiStore.getState().beginNavigation();
    useContextPaneStore.getState().reset();
    openWorkspaceView(view);
  };

  const handleSend = async (
    text: string,
    attachments?: Attachment[],
    submission?: ChatSendSubmission
  ) => {
    const workspace = productDataScope(useWorkspaceStore.getState().current);
    const conversation = useConversationStore.getState();
    const sourceConversationId = conversation.activeId;
    const sourceEpoch = conversation.newConversationEpoch;
    const targetIsCurrent = () =>
      sameProductDataScope(workspace, productDataScope(useWorkspaceStore.getState().current)) &&
      useConversationStore.getState().activeId === sourceConversationId &&
      useConversationStore.getState().newConversationEpoch === sourceEpoch;
    if (
      submission &&
      (useWorkspaceStore.getState().isLoading ||
        !sameProductDataScope(submission, workspace) ||
        submission.conversationId !== sourceConversationId ||
        submission.newConversationEpoch !== sourceEpoch)
    )
      return false;

    const parsed = parseGuiSlashCommand(text);
    if (
      attachments?.length &&
      (parsed?.command.target === 'side' || parsed?.command.target === 'goal')
    )
      throw new Error('目标与支线命令暂不支持附件，请移除附件后重试');

    if (!attachments?.length) {
      if (
        submission &&
        (submission.payload.text !== text || submission.payload.attachments?.length)
      )
        return false;
      // Local slash handlers may legitimately clear the selected conversation.
      // Observe intervening selections so that command success cannot adopt a
      // newer page or pretend to be an input journal receipt.
      let superseded = false;
      const unsubscribe = useConversationStore.subscribe((state) => {
        if (
          state.newConversationEpoch !== sourceEpoch ||
          (state.activeId !== sourceConversationId && state.activeId !== null)
        )
          superseded = true;
      });
      try {
        const command = dispatchGuiSlashCommand(text, {
          clear: clearCurrentChat,
          goal: async (argument) => {
            if (useWorkspaceStore.getState().isLoading || conversation.isLoading)
              throw new Error('请等待当前会话加载完成');
            const source: GoalCommandSource = {
              scope: workspace,
              conversationId: sourceConversationId,
              conversationEpoch: sourceEpoch,
              navigationEpoch: useUiStore.getState().navigationEpoch,
              requestId: submission?.externalId ?? crypto.randomUUID(),
              text: argument,
            };
            // 缓存缺失不等于没有目标；确认原后端记录后才能选择编辑或创建。
            const recorded = await recordedCommandGoal(source);
            const state = useTaskRuntimeStore.getState();
            const current = state.activeRun;
            if (recorded && !['completed', 'cancelled', 'failed'].includes(recorded.status)) {
              if (
                !current ||
                !state.taskScope ||
                !sameProductDataScope(workspace, state.taskScope) ||
                current.run_id !== recorded.run_id ||
                current.goal_revision !== recorded.goal_revision ||
                current.goal_sha256 !== recorded.goal_sha256 ||
                goalControls.current?.runId !== recorded.run_id
              ) {
                await state.loadByConversation(workspace.workspaceId, recorded.conversation_id);
                throw new Error(
                  useTaskRuntimeStore.getState().error ??
                    '当前目标已重新加载，请再次提交命令；草稿已保留'
                );
              }
              await goalControls.current.edit(argument);
            } else if (argument) {
              await requireCommandModel(source);
              const latest = await recordedCommandGoal(source);
              if (latest && !['completed', 'cancelled', 'failed'].includes(latest.status))
                throw new Error('当前会话已有活动目标，请重新提交命令修改；草稿已保留');
              const accepted = await sendMessage(
                argument,
                undefined,
                submission?.payload.domainPlugin,
                submission
                  ? { ...submission, payload: { ...submission.payload, text: argument } }
                  : undefined
              );
              if (!accepted) throw new Error('目标尚未被接受，草稿已保留，请核对错误后重试');
            } else {
              setGoalDraft(source);
            }
          },
          side: async (argument) => {
            if (
              useWorkspaceStore.getState().isLoading ||
              conversation.isLoading ||
              !sourceConversationId
            )
              throw new Error('请先打开或保存一个主对话，再创建支线');
            if (
              conversation.sideConversations.some(
                (entry) => entry.conversation_id === sourceConversationId
              )
            )
              throw new Error('支线对话不能再创建嵌套支线，请返回主对话');
            const source = {
              scope: workspace,
              conversationId: sourceConversationId,
              conversationEpoch: sourceEpoch,
              navigationEpoch: useUiStore.getState().navigationEpoch,
              requestId: submission?.externalId ?? crypto.randomUUID(),
              text: argument,
            };
            if (argument) {
              await requireCommandModel(source);
              await useConversationStore.getState().createSideConversation({
                requestId: source.requestId,
                prompt: argument,
                source,
                canProject: () => ownsCommandContext(source),
                // 创建回执先确认旧草稿，再由原 store 打开子会话；迟到回执不能清新页面草稿。
                onCreated: (canProject) => submission?.onDirectAccepted?.(canProject),
              });
            } else {
              setSideDraft(source);
            }
          },
          tasks: openTasks,
          analysis: () => openPrimaryWorkbench('analysis'),
          research: () => openPrimaryWorkbench('research'),
          browser: openBrowser,
          files: openFiles,
          workflows: () => openPrimaryWorkbench('workflow'),
          extract: () => openPrimaryWorkbench('extract'),
        });
        const commandEpoch = useUiStore.getState().navigationEpoch;
        if (await command) {
          const settledConversationId = useConversationStore.getState().activeId;
          const canProject = () =>
            !superseded &&
            sameProductDataScope(
              workspace,
              productDataScope(useWorkspaceStore.getState().current)
            ) &&
            useConversationStore.getState().newConversationEpoch === sourceEpoch &&
            useConversationStore.getState().activeId === settledConversationId &&
            useUiStore.getState().navigationEpoch === commandEpoch;
          try {
            void Promise.resolve(submission?.onDirectAccepted?.(canProject)).catch((error) =>
              console.warn('[ChatPanel] Local command draft acknowledgement failed:', error)
            );
          } catch (error) {
            console.warn('[ChatPanel] Local command draft acknowledgement failed:', error);
          }
          return true;
        }
      } finally {
        unsubscribe();
      }
    }
    if (activeSide && targetIsCurrent())
      noteSideConversationTurnStarted(activeSide.conversation_id);
    const sent = await sendMessage(text, attachments, submission?.payload.domainPlugin, submission);
    if (!sent && activeSide && targetIsCurrent()) {
      void useConversationStore.getState().init(activeSide.workspace_id);
    }
    return sent;
  };

  const handleSideModelChange = async (modelId: string) => {
    if (!activeSide) return;
    try {
      await updateSideConversationModel(activeSide.conversation_id, modelId || null);
    } catch (cause) {
      useToastStore
        .getState()
        .addToast('error', cause instanceof Error ? cause.message : String(cause));
    }
  };

  return (
    <>
      <AgentPane
        ariaLabel={activeSide ? '支线 Agent' : '主 Agent'}
        role="main"
        layout={messages.length === 0 && !pendingHitlRequests.length ? 'home' : 'conversation'}
        bodyRef={scrollRef}
        bodyRole="log"
        bodyAriaLive="polite"
        bodyAriaLabel="消息列表"
        onBodyScroll={handleScroll}
        header={
          <>
            <div className="min-w-0 pl-9">
              <div className="truncate text-[13px] font-medium text-[var(--text-primary)]">
                {activeSide
                  ? activeSide.title
                  : activeConversation?.title || currentWorkspace?.name || 'Eclip'}
              </div>
              <div className="truncate text-[10px] text-[var(--text-tertiary)]">
                {activeSide
                  ? activeSide.launch_error
                    ? 'Side Conversation · 首轮待重试'
                    : `Side Conversation · 已提交快照 ${activeSide.snapshot_message_count} 条`
                  : currentWorkspace
                    ? workspaceLocationLabel(currentWorkspace)
                    : '新建任务，或选择一个工作空间'}
              </div>
            </div>
            <div className="ml-auto flex items-center gap-2 text-xs text-[var(--text-tertiary)]">
              {activeSide && (
                <label
                  className="relative flex h-7 w-7 shrink-0 items-center justify-center rounded-md text-[var(--text-tertiary)] hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)] focus-within:ring-1 focus-within:ring-[var(--accent)] sm:w-auto sm:bg-transparent sm:hover:bg-transparent"
                  title="支线对话模型"
                >
                  <Bot size={15} className="sm:hidden" aria-hidden="true" />
                  <select
                    value={activeSide.model_id ?? ''}
                    onChange={(event) => void handleSideModelChange(event.target.value)}
                    aria-label="支线对话模型"
                    className="absolute inset-0 cursor-pointer opacity-0 sm:static sm:h-7 sm:max-w-44 sm:rounded-md sm:border sm:border-[var(--border-primary)] sm:bg-[var(--bg-secondary)] sm:px-2 sm:text-[11px] sm:text-[var(--text-secondary)] sm:opacity-100 sm:outline-none sm:focus:border-[var(--accent)]"
                  >
                    <option value="">继承主对话</option>
                    {configuredModels.map((model) => (
                      <option key={model.id} value={model.id}>
                        {modelLabel(model, modelProviders)}
                      </option>
                    ))}
                  </select>
                </label>
              )}
              <div className="hidden items-center gap-2 sm:flex">
                <span className="h-2 w-2 rounded-full bg-[var(--accent)]" />
                <span>{isSubmitting ? '正在提交' : runStatusLabel(runStatus, isStreaming)}</span>
              </div>
              <div ref={workspaceMenuRef} className="relative">
                <button
                  type="button"
                  onClick={() => setWorkspaceMenuOpen((open) => !open)}
                  className="flex h-7 w-7 items-center justify-center rounded-md text-[var(--text-tertiary)] transition-colors hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]"
                  title="打开工作台"
                  aria-label="打开工作台菜单"
                  aria-expanded={workspaceMenuOpen}
                >
                  <MoreHorizontal size={15} />
                </button>
                {workspaceMenuOpen && (
                  <WorkspaceMenu
                    onSelect={(action) => {
                      action();
                      setWorkspaceMenuOpen(false);
                    }}
                    openAnalysis={() => openPrimaryWorkbench('analysis')}
                    openResearch={() => openPrimaryWorkbench('research')}
                    openBrowser={openBrowser}
                    openFiles={openFiles}
                    openWorkflow={() => openPrimaryWorkbench('workflow')}
                    openExtract={() => openPrimaryWorkbench('extract')}
                  />
                )}
              </div>
              <button
                type="button"
                onClick={openTasks}
                className={`relative flex h-7 w-7 items-center justify-center rounded-md transition-colors hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)] ${contextTarget?.kind === 'tasks' ? 'bg-[var(--bg-hover)] text-[var(--text-primary)]' : 'text-[var(--text-tertiary)]'}`}
                title="任务运行"
                aria-label="打开任务运行"
              >
                <PanelRightOpen size={15} />
                {todoCount > 0 && (
                  <span className="absolute -right-0.5 -top-0.5 min-w-3 rounded-full bg-[var(--accent)] px-0.5 text-center text-[8px] leading-3 text-white">
                    {todoCount > 9 ? '9+' : todoCount}
                  </span>
                )}
              </button>
            </div>
          </>
        }
        footer={
          <div className={messages.length === 0 ? 'pt-2' : 'pt-3'}>
            <div className={`w-full ${CHAT_GUTTER}`}>
              <HitlDialog
                requests={pendingHitlRequests}
                conversationLabel={pendingConversationLabel}
                onApproval={(request, approved, reason, scope) =>
                  sendApproval(request.identity, approved, reason, scope)
                }
                onInput={(request, answer) => sendInput(request.identity, answer)}
                onSelection={(request, selection, instructions) =>
                  sendSelection(request.identity, selection, instructions)
                }
              />
            </div>
            {queuedInputs.length > 0 && (
              <QueuedInputList
                key={JSON.stringify([
                  productDataScope(currentWorkspace),
                  activeConversationId,
                  conversationEpoch,
                ])}
                items={queuedInputs.filter(
                  (item) =>
                    item.workspaceId === productDataScope(currentWorkspace).workspaceId &&
                    item.conversationId === activeConversationId
                )}
                onRemove={removeQueuedMessage}
                onAbandonChannelInput={abandonChannelRecoveryInput}
                onReplace={replaceQueuedMessage}
                onReorder={reorderQueuedMessage}
                onSteer={steerQueuedMessage}
              />
            )}
            {activeTaskRun &&
              !['completed', 'cancelled'].includes(activeTaskRun.status) &&
              activeTaskRun.conversation_id === activeConversationId &&
              taskScope?.workspaceId === activeTaskRun.workspace_id &&
              sameProductDataScope(taskScope, productDataScope(currentWorkspace)) && (
                <GoalBar
                  key={JSON.stringify([
                    taskScope.workspaceId,
                    taskScope.workspaceGeneration,
                    activeConversationId,
                    conversationEpoch,
                    activeTaskRun.run_id,
                  ])}
                  run={activeTaskRun}
                  scope={taskScope}
                  conversationEpoch={conversationEpoch}
                  controlsRef={goalControls}
                />
              )}
            <ChatInput
              onSend={handleSend}
              isStreaming={isStreaming}
              onCancel={cancel}
              queuedCount={queuedInputs.length}
              home={timelineMessages.length === 0}
            />
          </div>
        }
      >
        {agentInputReadError && (
          <div
            role="status"
            className={`w-full pt-2 text-xs text-[var(--color-error)] ${CHAT_GUTTER}`}
          >
            跨会话消息读取失败，已有记录保留：{agentInputReadError}
          </div>
        )}
        {timelineMessages.length === 0 ? (
          <WelcomeScreen />
        ) : (
          <div className={`w-full ${CHAT_GUTTER}`}>
            <div className="space-y-1 pb-3 pt-2">
              <ConversationTimeline
                messages={timelineMessages}
                targetScope={{
                  workspaceId: productDataScope(currentWorkspace).workspaceId,
                  conversationId: activeConversationId,
                }}
                containerKey={JSON.stringify([
                  productDataScope(currentWorkspace),
                  activeConversationId,
                ])}
                onRegenerate={handleRegenerate}
                onEditAndResend={handleEditAndResend}
                onFork={activeConversationId ? handleFork : undefined}
                forkReplies={forkReplies}
                forkDisabled={isStreaming || !!forkSource}
              />

              <ExecutionStatus />

              {isStreaming &&
                !hasActivity &&
                !messages.some(
                  (m) =>
                    m.isStreaming &&
                    (m.content.trim() ||
                      m.assistantBlocks?.some(
                        (view) =>
                          view.block.phase !== 'final_answer' &&
                          nativeAssistantBlockText(view).trim()
                      ))
                ) && (
                  <div className="flex items-center gap-3 px-1 py-3">
                    <div className="spinner" />
                    <span className="text-xs text-[var(--text-tertiary)] animate-breathe">
                      {runStatusLabel(runStatus, true)}
                    </span>
                  </div>
                )}

              {isCancelled && (
                <div className="flex items-center gap-3 py-3">
                  <div className="h-px flex-1 bg-[var(--border-secondary)]" />
                  <span className="text-xs italic text-[var(--text-tertiary)]">已停止响应</span>
                  <div className="h-px flex-1 bg-[var(--border-secondary)]" />
                </div>
              )}

              {!failureToastDismissed && (
                <div className="py-1">
                  <FailureToast onDismiss={() => setFailureToastDismissed(true)} />
                </div>
              )}
            </div>
            <div ref={bottomRef} className="h-1" />
          </div>
        )}
      </AgentPane>
      {forkSource && <ReplyForkDialog source={forkSource} onClose={() => setForkSource(null)} />}
      {goalDraft && (
        <GoalCreationDialog
          key={goalDraft.requestId}
          source={goalDraft}
          onClose={() =>
            setGoalDraft((current) => (current?.requestId === goalDraft.requestId ? null : current))
          }
          onBind={(bound) =>
            setGoalDraft((current) => (current?.requestId === bound.requestId ? bound : current))
          }
          onSend={sendMessage}
        />
      )}
      {sideDraft && (
        <SideConversationDialog
          key={sideDraft.requestId}
          isOpen
          source={sideDraft}
          initialPrompt={sideDraft.text}
          onClose={() =>
            setSideDraft((current) => (current?.requestId === sideDraft.requestId ? null : current))
          }
        />
      )}
    </>
  );
}

async function recordedCommandGoal(source: GoalCommandSource) {
  const recorded = source.conversationId
    ? await taskRuntimeApi.latestRunForConversation(
        source.scope.workspaceId,
        source.conversationId,
        source.scope.workspaceGeneration
      )
    : null;
  if (!ownsCommandSource(source) || useConversationStore.getState().isLoading)
    throw new Error('目标所属会话或工作空间已变化');
  if (
    recorded &&
    (recorded.workspace_id !== source.scope.workspaceId ||
      recorded.conversation_id !== source.conversationId)
  )
    throw new Error('目标记录不属于当前会话，请刷新后重试');
  return recorded;
}

async function requireCommandModel(source: GoalCommandSource | SideConversationSource) {
  const response = await configuredModelPresentation();
  if (!ownsCommandSource(source) || useConversationStore.getState().isLoading)
    throw new Error('命令所属会话或工作空间已变化，请重新提交');
  if (!response.models.some((model) => model.enabled))
    throw new Error('请先配置可用模型，再创建目标或支线对话');
}

function GoalCreationDialog({
  source,
  onClose,
  onSend,
  onBind,
}: {
  source: GoalCommandSource;
  onClose: () => void;
  onSend: ReturnType<typeof useTauriChat>['sendMessage'];
  onBind: (source: GoalCommandSource) => void;
}) {
  const [text, setText] = useState(source.text);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const inputRef = useRef<HTMLTextAreaElement>(null);
  const mounted = useRef(false);
  const pending = useRef(false);
  const submitted = useRef<{ text: string; externalId: string } | null>(null);
  const boundSource = useRef(source);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);
  const create = async () => {
    const goal = text.trim();
    if (!goal || pending.current) return;
    const currentSource = boundSource.current;
    if (!ownsCommandSource(currentSource) || useConversationStore.getState().isLoading) {
      setError('目标所属会话或工作空间已变化，请保留草稿并重新打开');
      return;
    }
    pending.current = true;
    setBusy(true);
    setError(null);
    try {
      await requireCommandModel(currentSource);
      if (!mounted.current) return;
      const recorded = await recordedCommandGoal(currentSource);
      if (!mounted.current) return;
      if (recorded && !['completed', 'cancelled', 'failed'].includes(recorded.status))
        throw new Error('当前会话已有活动目标，请使用 /goal 修改；创建草稿已保留');
      if (submitted.current?.text !== goal)
        submitted.current = { text: goal, externalId: crypto.randomUUID() };
      const intent = submitted.current;
      let accepted = false;
      const confirm = (canProject: () => boolean) => {
        accepted = true;
        if (mounted.current && canProject() && ownsCommandSource(boundSource.current)) onClose();
      };
      // 与普通输入共用原提交身份和准入路径；不另建后台 Task 或推测 Goal 已创建。
      const sent = await onSend(goal, undefined, undefined, {
        ...currentSource.scope,
        conversationId: currentSource.conversationId,
        newConversationEpoch: currentSource.conversationEpoch,
        externalId: intent.externalId,
        payload: { text: goal },
        onDirectAccepted: confirm,
        onPersisted: (_receipt, canProject) => confirm(canProject),
        onConversationCreated: (address) => {
          if (
            !mounted.current ||
            currentSource.conversationId !== null ||
            address.workspace_id !== currentSource.scope.workspaceId ||
            !ownsCommandSource(currentSource)
          )
            return;
          const bound = { ...currentSource, conversationId: address.conversation_id };
          boundSource.current = bound;
          onBind(bound);
        },
      });
      if (mounted.current && !accepted) {
        setError(
          sent
            ? '输入已提交，目标状态等待后端确认；请从当前会话运行核对，勿重复创建。'
            : '目标尚未被接受，草稿已保留，请核对错误后重试'
        );
      }
    } catch (cause) {
      if (mounted.current) setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      pending.current = false;
      if (mounted.current) setBusy(false);
    }
  };
  return (
    <Modal
      ariaLabel="创建目标"
      initialFocusRef={inputRef}
      onClose={() => {
        if (!pending.current) onClose();
      }}
      className="w-[min(560px,calc(100vw-2rem))] rounded-lg border border-[var(--border-primary)] bg-[var(--bg-primary)] p-4 shadow-[var(--shadow-xl)]"
    >
      <h2 className="text-sm font-semibold text-[var(--text-primary)]">创建并开始目标</h2>
      <p className="mt-2 text-xs text-[var(--text-secondary)]">
        确认后在当前会话开始处理；目标状态以任务运行记录为准。
      </p>
      <label className="mt-3 block text-xs text-[var(--text-secondary)]">
        目标内容
        <textarea
          ref={inputRef}
          value={text}
          onChange={(event) => setText(event.target.value)}
          disabled={busy}
          rows={5}
          className="mt-1 w-full rounded border border-[var(--border-primary)] bg-[var(--bg-secondary)] p-2 text-sm text-[var(--text-primary)]"
        />
      </label>
      {error && (
        <p role="alert" className="mt-2 text-xs text-[var(--color-error)]">
          {error}
        </p>
      )}
      <div className="mt-3 flex justify-end gap-3 text-xs">
        <button type="button" onClick={onClose} disabled={busy}>
          取消
        </button>
        <button
          type="button"
          onClick={() => void create()}
          disabled={busy || !text.trim()}
          className="text-[var(--accent)]"
        >
          {busy ? '正在提交…' : '创建并开始'}
        </button>
      </div>
    </Modal>
  );
}

// Only the final visible message of a user turn can offer Fork. The backend
// independently verifies its canonical prefix and replacement admission.
export function completedReplyForks(messages: readonly ChatMessage[]): Map<string, number> {
  const replies = new Map<string, number>();
  let userTurnIndex = -1;
  let lastVisible: ChatMessage | undefined;
  const finish = () => {
    if (
      userTurnIndex >= 0 &&
      lastVisible?.role === 'assistant' &&
      lastVisible.finalAnswerConfirmed === true &&
      !lastVisible.isStreaming &&
      lastVisible.content.trim()
    ) {
      replies.set(lastVisible.id, userTurnIndex);
    }
  };
  for (const message of messages) {
    if (message.internalAgent) continue;
    if (message.role === 'user') {
      finish();
      userTurnIndex += 1;
    }
    lastVisible = message;
  }
  finish();
  return replies;
}

// UI observation is bounded; only the backend Goal CAS proves physical quiescence.
const GOAL_WAIT_MILLISECONDS = 30_000;
const GOAL_POLL_MILLISECONDS = 250;

type GoalBarControls = { runId: string; edit: (text: string) => Promise<void> };

function GoalBar({
  run,
  scope,
  conversationEpoch,
  controlsRef,
}: {
  run: TaskRun;
  scope: ProductDataScope;
  conversationEpoch: number;
  controlsRef?: RefObject<GoalBarControls | null>;
}) {
  const [editing, setEditing] = useState(false);
  const [goal, setGoal] = useState(run.goal);
  const [reason, setReason] = useState('用户更新目标');
  const [editRevision, setEditRevision] = useState(run.goal_revision);
  const [progress, setProgress] = useState<string | null>(null);
  const [notice, setNotice] = useState<{
    text: string;
    failed: boolean;
  } | null>(null);
  const mounted = useRef(false);
  const operation = useRef<symbol | null>(null);
  const editInput = useRef<HTMLInputElement>(null);
  useEffect(() => {
    if (editing) editInput.current?.focus();
  }, [editing]);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      operation.current = null;
    };
  }, []);
  useEffect(() => {
    if (!editing && !progress) {
      setGoal(run.goal);
      setEditRevision(run.goal_revision);
    }
  }, [run.goal, run.goal_revision, editing, progress]);
  const target = {
    workspace_id: run.workspace_id,
    workspace_generation: scope.workspaceGeneration,
    run_id: run.run_id,
  };
  const ownsView = () => {
    const conversation = useConversationStore.getState();
    const workspace = useWorkspaceStore.getState();
    const state = useTaskRuntimeStore.getState();
    return (
      mounted.current &&
      !workspace.isLoading &&
      !conversation.isLoading &&
      sameProductDataScope(scope, productDataScope(workspace.current)) &&
      !!state.taskScope &&
      sameProductDataScope(scope, state.taskScope) &&
      conversation.activeId === run.conversation_id &&
      conversation.newConversationEpoch === conversationEpoch &&
      state.activeRun?.run_id === run.run_id &&
      state.activeRun.workspace_id === run.workspace_id &&
      state.activeRun.conversation_id === run.conversation_id
    );
  };
  const readCurrent = (checkError = true) => {
    if (!ownsView()) throw new Error('目标所属会话或工作空间已变化');
    const state = useTaskRuntimeStore.getState();
    if (checkError && state.error) throw new Error(state.error);
    if (!state.activeRun) throw new Error('当前目标已不可用');
    return state.activeRun;
  };
  const refreshCurrent = async () => {
    if (!ownsView()) throw new Error('目标所属会话或工作空间已变化');
    await useTaskRuntimeStore.getState().refresh(run.workspace_id, run.run_id);
    return readCurrent();
  };
  const save = async (
    draft = goal,
    revision = editRevision,
    changeReason = reason
  ): Promise<boolean> => {
    const text = draft.trim();
    if (!text || operation.current || !ownsView()) return false;
    const token = Symbol('goal-save');
    operation.current = token;
    setNotice(null);
    const deadline = performance.now() + GOAL_WAIT_MILLISECONDS;
    try {
      let current = readCurrent(false);
      if (current.goal_revision !== revision)
        throw new Error('目标版本已变化，请保留草稿并重新核对后编辑');
      if (current.status !== 'paused') {
        setProgress('正在请求暂停任务…');
        const receipt = await useTaskRuntimeStore.getState().pause(target);
        if (!ownsView()) return false;
        if (!receipt?.success || receipt.run_id !== run.run_id)
          throw new Error(useTaskRuntimeStore.getState().error ?? '暂停任务未被后端接受');
      }
      for (;;) {
        if (!ownsView()) return false;
        setProgress('正在等待任务安全点…');
        current = await refreshCurrent();
        if (current.goal_revision !== revision)
          throw new Error('目标版本已变化，请保留草稿并重新核对后编辑');
        if (['completed', 'cancelled', 'failed'].includes(current.status))
          throw new Error('运行状态已变化，目标尚未更新');
        const state = useTaskRuntimeStore.getState();
        const hasVisibleActivity =
          !!state.continuation?.active_turn ||
          state.backgroundCells.some((cell) =>
            ['prepared', 'queued', 'running'].includes(cell.phase)
          );
        if (current.status === 'paused' && !hasVisibleActivity) {
          setProgress('正在提交目标更新…');
          let accepted = false;
          let waitingForOwner = false;
          try {
            accepted = await useTaskRuntimeStore
              .getState()
              .updateGoal(target, revision, text, changeReason.trim() || '用户更新目标');
          } catch (error) {
            if (!ownsView()) return false;
            // These exact rejections occur before any Goal write in the original CAS.
            // Do not retry conflicts, I/O failures, or an unconfirmed committed update.
            const message = error instanceof Error ? error.message : String(error);
            const prefix = `goal update rejected for run ${run.run_id}: `;
            const busyReasons = [
              'run still has an active driver',
              'run still has an active RunTurn',
              'run still has an active Subagent attempt',
              'run still has an active command cell',
            ];
            if (!busyReasons.some((item) => message === prefix + item)) throw error;
            waitingForOwner = true;
          }
          if (!ownsView()) return false;
          if (accepted) {
            setProgress('正在确认目标记录…');
            const updated = await refreshCurrent();
            if (updated.goal_revision !== revision + 1 || updated.goal !== text)
              throw new Error('目标提交后的记录尚未确认，请刷新核对；草稿已保留');
            setEditing(false);
            setNotice({
              text: '目标已更新；旧任务计划与证据已失效，需要重新校验。',
              failed: false,
            });
            return true;
          }
          if (!waitingForOwner)
            throw new Error(
              useTaskRuntimeStore.getState().error ?? '目标更新未被后端接受，请检查版本后重试'
            );
        }
        if (performance.now() >= deadline)
          throw new Error('等待安全点超时；暂停请求仍有效，目标尚未更新，草稿已保留');
        await new Promise<void>((resolve) => setTimeout(resolve, GOAL_POLL_MILLISECONDS));
      }
    } catch (error) {
      if (ownsView())
        setNotice({
          text: error instanceof Error ? error.message : String(error),
          failed: true,
        });
      return false;
    } finally {
      if (mounted.current && operation.current === token) {
        operation.current = null;
        setProgress(null);
      }
    }
  };
  useImperativeHandle(controlsRef, () => ({
    runId: run.run_id,
    async edit(text) {
      if (!ownsView() || operation.current) throw new Error('当前目标正在处理或已切换，请稍后重试');
      setGoal(text || run.goal);
      setEditRevision(run.goal_revision);
      setReason('用户更新目标');
      setNotice(null);
      setEditing(true);
      if (text && !(await save(text, run.goal_revision, '用户更新目标')))
        throw new Error('目标尚未更新，请查看目标栏；草稿已保留');
    },
  }));
  const endGoal = async () => {
    if (operation.current || !ownsView()) return;
    const token = Symbol('goal-end');
    operation.current = token;
    setProgress('正在结束目标…');
    setNotice(null);
    try {
      const receipt = await useTaskRuntimeStore.getState().cancel(target);
      if (!ownsView()) return;
      if (!receipt?.success || receipt.run_id !== run.run_id)
        throw new Error(
          useTaskRuntimeStore.getState().error ?? '目标结束请求未被后端接受，请检查运行状态后重试'
        );
      const current = await refreshCurrent();
      if (!ownsView()) return;
      setNotice({
        text: current.status === 'cancelled' ? '目标已结束' : '结束请求已接受，等待运行结算',
        failed: false,
      });
    } catch (error) {
      if (ownsView())
        setNotice({
          text: error instanceof Error ? error.message : String(error),
          failed: true,
        });
    } finally {
      if (mounted.current && operation.current === token) {
        operation.current = null;
        setProgress(null);
      }
    }
  };
  return (
    <section
      className={`mb-2 w-full text-xs ${CHAT_GUTTER}`}
      aria-label="当前目标"
      aria-busy={!!progress}
    >
      <div className="flex items-center gap-2">
        <span className="shrink-0 text-[var(--text-tertiary)]">Goal r{run.goal_revision}</span>
        {editing ? (
          <>
            <input
              ref={editInput}
              value={goal}
              onChange={(event) => setGoal(event.target.value)}
              disabled={!!progress}
              className="min-w-0 flex-1 rounded border border-[var(--border-primary)] bg-[var(--bg-primary)] px-2 py-1 text-[var(--text-primary)]"
              aria-label="编辑当前目标"
            />
            <input
              value={reason}
              onChange={(event) => setReason(event.target.value)}
              disabled={!!progress}
              className="hidden w-32 rounded border border-[var(--border-primary)] bg-[var(--bg-primary)] px-2 py-1 text-[var(--text-secondary)] sm:block"
              aria-label="目标修改原因"
            />
            <button
              type="button"
              onClick={() => void save()}
              disabled={!!progress || !goal.trim()}
              className="text-[var(--accent)]"
            >
              保存
            </button>
            <button
              type="button"
              onClick={() => setEditing(false)}
              disabled={!!progress}
              className="text-[var(--text-tertiary)]"
            >
              取消
            </button>
          </>
        ) : (
          <>
            <span className="min-w-0 flex-1 truncate text-[var(--text-secondary)]" title={run.goal}>
              {run.goal}
            </span>
            <button
              type="button"
              onClick={() => {
                setGoal(run.goal);
                setEditRevision(run.goal_revision);
                setNotice(null);
                setEditing(true);
              }}
              disabled={!!progress}
              className="text-[var(--text-tertiary)] hover:text-[var(--accent)]"
              aria-label="编辑当前目标"
            >
              编辑
            </button>
            <button
              type="button"
              onClick={() => void endGoal()}
              disabled={!!progress}
              className="text-[var(--color-error)]"
              aria-label="结束并删除当前目标"
            >
              结束目标
            </button>
          </>
        )}
      </div>
      {progress && (
        <p className="mt-1 text-[var(--text-secondary)]" role="status" aria-live="polite">
          {progress}
        </p>
      )}
      {notice && (
        <p
          className={`mt-1 ${notice.failed ? 'text-[var(--color-error)]' : 'text-[var(--text-tertiary)]'}`}
          role={notice.failed ? 'alert' : 'status'}
        >
          {notice.text}
        </p>
      )}
    </section>
  );
}

function WorkspaceMenu({
  onSelect,
  openAnalysis,
  openResearch,
  openBrowser,
  openFiles,
  openWorkflow,
  openExtract,
}: {
  onSelect: (action: () => void) => void;
  openAnalysis: () => void;
  openResearch: () => void;
  openBrowser: () => void;
  openFiles: () => void;
  openWorkflow: () => void;
  openExtract: () => void;
}) {
  const items = [
    { label: '分析', icon: <FlaskConical size={13} />, action: openAnalysis },
    { label: '研究', icon: <BookOpen size={13} />, action: openResearch },
    { label: '浏览器', icon: <Globe2 size={13} />, action: openBrowser },
    { label: '文件', icon: <FileCode size={13} />, action: openFiles },
    { label: '工作流', icon: <Workflow size={13} />, action: openWorkflow },
    { label: '结构化提取', icon: <FileJson size={13} />, action: openExtract },
  ];
  return (
    <div
      role="menu"
      aria-label="工作台"
      className="absolute right-0 top-9 z-50 w-40 overflow-hidden rounded-md border border-[var(--border-primary)] bg-[var(--bg-primary)] py-1 shadow-[var(--shadow-md)]"
    >
      {items.map((item) => (
        <button
          key={item.label}
          type="button"
          role="menuitem"
          onClick={() => onSelect(item.action)}
          className="flex h-8 w-full items-center gap-2 px-2.5 text-left text-xs text-[var(--text-secondary)] hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]"
        >
          <span className="text-[var(--text-tertiary)]">{item.icon}</span>
          <span>{item.label}</span>
        </button>
      ))}
    </div>
  );
}

function queuedInputVersion(item: QueuedChatInput): string {
  return JSON.stringify([
    item.identity.address.workspace_id,
    item.identity.address.conversation_id,
    item.identity.input_id,
    item.identity.revision,
    item.identity.payload_sha256,
    !!item.channelOwned,
  ]);
}

function QueuedInputList({
  items,
  onRemove,
  onAbandonChannelInput,
  onReplace,
  onReorder,
  onSteer,
}: {
  items: QueuedChatInput[];
  onRemove: (id: string) => void;
  onAbandonChannelInput: (id: string) => void;
  onReplace: (id: string, text: string) => Promise<boolean>;
  onReorder: (sourceId: string, targetId: string) => void;
  onSteer: (id: string) => Promise<boolean>;
}) {
  const [draggedId, setDraggedId] = useState<string | null>(null);
  const [editing, setEditing] = useState<{
    id: string;
    version: string;
  } | null>(null);
  const [editingText, setEditingText] = useState('');
  const [busyId, setBusyId] = useState<string | null>(null);
  const [feedback, setFeedback] = useState<{
    id: string;
    version: string;
    text: string;
    failed: boolean;
  } | null>(null);
  const live = useRef(false);
  const request = useRef<symbol | null>(null);
  const latestItems = useRef(items);
  latestItems.current = items;
  useEffect(() => {
    live.current = true;
    return () => {
      live.current = false;
      request.current = null;
    };
  }, []);
  const stillOwnsItem = (id: string, version: string) =>
    live.current &&
    latestItems.current.some(
      (item) => item.id === id && queuedInputVersion(item) === version && !item.channelOwned
    );
  const saveEdit = async (item: QueuedChatInput) => {
    if (!editing || request.current || item.channelOwned) return;
    const original = editing;
    if (!stillOwnsItem(original.id, original.version)) {
      setFeedback({
        id: item.id,
        version: queuedInputVersion(item),
        text: '排队消息已变化，请保留草稿并重新核对后编辑',
        failed: true,
      });
      return;
    }
    const token = Symbol('steer-edit');
    request.current = token;
    setBusyId(item.id);
    setFeedback(null);
    try {
      const accepted = await onReplace(original.id, editingText);
      if (!live.current || request.current !== token) return;
      if (accepted) {
        setEditing(null);
        return;
      }
      if (!stillOwnsItem(original.id, original.version)) return;
      setFeedback({
        ...original,
        text: '编辑未被接受，草稿已保留；请刷新队列后重试',
        failed: true,
      });
    } catch (error) {
      if (stillOwnsItem(original.id, original.version))
        setFeedback({
          ...original,
          text: error instanceof Error ? error.message : String(error),
          failed: true,
        });
    } finally {
      if (live.current && request.current === token) {
        request.current = null;
        setBusyId(null);
      }
    }
  };
  const execute = async (item: QueuedChatInput) => {
    if (request.current || item.channelOwned) return;
    const version = queuedInputVersion(item);
    const token = Symbol('steer-execute');
    request.current = token;
    setBusyId(item.id);
    setFeedback(null);
    try {
      const accepted = await onSteer(item.id);
      if (stillOwnsItem(item.id, version))
        setFeedback({
          id: item.id,
          version,
          text: accepted ? '已送达当前任务' : '尚未确认送达，请查看输入状态或错误后重试',
          failed: false,
        });
    } catch (error) {
      if (stillOwnsItem(item.id, version))
        setFeedback({
          id: item.id,
          version,
          text: error instanceof Error ? error.message : String(error),
          failed: true,
        });
    } finally {
      if (live.current && request.current === token) {
        request.current = null;
        setBusyId(null);
      }
    }
  };
  return (
    <section className={`mb-2 w-full ${CHAT_GUTTER}`} aria-label="Steer 队列">
      <div className="mb-1 flex items-center justify-between px-1 text-[11px] text-[var(--text-tertiary)]">
        <span>Steer · {items.length} 条</span>
        <span>拖动或用上下移按钮调整顺序</span>
      </div>
      <div className="max-h-40 space-y-1 overflow-y-auto">
        {items.map((item, index) => (
          <div
            key={item.id}
            role="group"
            aria-label={`Steer ${index + 1}`}
            draggable={!item.channelOwned && !busyId && editing?.id !== item.id}
            onDragStart={(event) => {
              if (item.channelOwned || busyId) {
                event.preventDefault();
                return;
              }
              setDraggedId(item.id);
            }}
            onDragEnd={() => setDraggedId(null)}
            onDragOver={(event) => {
              if (!item.channelOwned) event.preventDefault();
            }}
            onDrop={(event) => {
              event.preventDefault();
              const source = latestItems.current.find((entry) => entry.id === draggedId);
              if (source && !source.channelOwned && !item.channelOwned && !busyId)
                onReorder(source.id, item.id);
              setDraggedId(null);
            }}
            className="border-l-2 border-[var(--border-primary)] bg-[var(--bg-secondary)] px-2 py-1.5 text-xs transition-colors hover:bg-[var(--bg-hover)]"
            style={{ opacity: draggedId === item.id ? 0.55 : 1 }}
          >
            <div className="flex min-h-9 items-center gap-2">
              <GripVertical
                size={14}
                className="shrink-0 cursor-grab text-[var(--text-tertiary)] active:cursor-grabbing"
                aria-hidden="true"
              />
              <span className="w-5 shrink-0 text-center tabular-nums text-[var(--text-tertiary)]">
                {index + 1}
              </span>
              <span className="min-w-0 flex-1 truncate text-[var(--text-secondary)]">
                {editing?.id === item.id ? (
                  <input
                    autoFocus
                    value={editingText}
                    disabled={!!busyId || item.channelOwned}
                    onChange={(event) => setEditingText(event.target.value)}
                    onKeyDown={(event) => {
                      // IME confirmation belongs to text entry, including WebKit's
                      // composition-end Enter where only keyCode 229 is retained.
                      if (event.nativeEvent.isComposing || event.nativeEvent.keyCode === 229)
                        return;
                      if (event.key === 'Escape' && !busyId) setEditing(null);
                      if (event.key === 'Enter') {
                        event.preventDefault();
                        void saveEdit(item);
                      }
                    }}
                    className="w-full rounded border border-[var(--border-primary)] bg-[var(--bg-primary)] px-1.5 py-0.5 text-xs text-[var(--text-primary)]"
                    aria-label="编辑排队消息"
                  />
                ) : (
                  item.text || item.attachments?.map((file) => file.name).join(', ') || '附件'
                )}
              </span>
              <span className="shrink-0 text-[10px] text-[var(--text-tertiary)]">
                {item.channelOwned
                  ? '渠道处理'
                  : queuedInputPhaseLabel(item.phase, item.drained, item.steerOutcome)}
              </span>
              <button
                type="button"
                onClick={() => {
                  const target = index > 0 ? items.at(index - 1) : undefined;
                  if (target && !target.channelOwned) onReorder(item.id, target.id);
                }}
                disabled={
                  !!busyId ||
                  item.channelOwned ||
                  index === 0 ||
                  !!items.at(index - 1)?.channelOwned
                }
                className="flex h-7 w-7 shrink-0 items-center justify-center text-[var(--text-tertiary)] hover:text-[var(--accent)] disabled:opacity-40"
                title="上移 Steer"
                aria-label="上移 Steer"
              >
                <ArrowUp size={14} />
              </button>
              <button
                type="button"
                onClick={() => {
                  const target = items.at(index + 1);
                  if (target && !target.channelOwned) onReorder(item.id, target.id);
                }}
                disabled={
                  !!busyId ||
                  item.channelOwned ||
                  index === items.length - 1 ||
                  !!items.at(index + 1)?.channelOwned
                }
                className="flex h-7 w-7 shrink-0 items-center justify-center text-[var(--text-tertiary)] hover:text-[var(--accent)] disabled:opacity-40"
                title="下移 Steer"
                aria-label="下移 Steer"
              >
                <ArrowDown size={14} />
              </button>
              {editing?.id === item.id ? (
                <>
                  <button
                    type="button"
                    onClick={() => void saveEdit(item)}
                    disabled={!!busyId || item.channelOwned}
                    aria-label="保存 Steer 编辑"
                  >
                    保存
                  </button>
                  <button
                    type="button"
                    onClick={() => setEditing(null)}
                    disabled={!!busyId}
                    aria-label="取消 Steer 编辑"
                  >
                    取消
                  </button>
                </>
              ) : (
                <button
                  type="button"
                  onClick={() => {
                    setEditing({
                      id: item.id,
                      version: queuedInputVersion(item),
                    });
                    setEditingText(item.text);
                    setFeedback(null);
                  }}
                  disabled={!!busyId || item.channelOwned}
                  className="flex h-7 w-7 shrink-0 items-center justify-center text-[var(--text-tertiary)] hover:text-[var(--accent)]"
                  title="编辑排队消息"
                  aria-label="编辑排队消息"
                >
                  ✎
                </button>
              )}
              <button
                type="button"
                onClick={() => void execute(item)}
                disabled={!!busyId || item.channelOwned}
                className="flex h-7 w-7 shrink-0 items-center justify-center text-[var(--text-tertiary)] hover:text-[var(--accent)]"
                title="立即执行 Steer"
                aria-label="立即执行 Steer"
              >
                <CornerUpLeft size={14} />
              </button>
              {item.channelOwned && item.phase === 'recovery_required' && (
                <button
                  type="button"
                  onClick={() => onAbandonChannelInput(item.id)}
                  disabled={!!busyId}
                  className="shrink-0 rounded border border-[var(--border-primary)] px-2 py-0.5 text-[11px] text-[var(--text-secondary)] hover:text-[var(--color-error)]"
                  title="仅解除队列阻塞；不撤销已发生的执行、发送或 UNKNOWN 结论"
                  aria-label="放弃这条待恢复消息"
                >
                  放弃这条待恢复消息
                </button>
              )}
              <button
                type="button"
                onClick={() => onRemove(item.id)}
                disabled={!!busyId || item.channelOwned}
                className="flex h-7 w-7 shrink-0 items-center justify-center text-[var(--text-tertiary)] hover:text-[var(--color-error)]"
                title="移出队列"
                aria-label="移出队列"
              >
                <X size={14} />
              </button>
            </div>
            {busyId === item.id && (
              <p role="status" aria-live="polite">
                正在处理 Steer…
              </p>
            )}
            {feedback?.id === item.id && feedback.version === queuedInputVersion(item) && (
              <p role={feedback.failed ? 'alert' : 'status'}>{feedback.text}</p>
            )}
          </div>
        ))}
      </div>
    </section>
  );
}

function queuedInputPhaseLabel(
  phase: QueuedChatInput['phase'],
  drained: boolean,
  steerOutcome?: QueuedChatInput['steerOutcome']
): string {
  if (steerOutcome === 'rejected') return '已拒绝';
  if (steerOutcome === 'retained') return '已保留';
  if (drained) return '已送达';
  switch (phase) {
    case 'persisted':
      return '已排队';
    case 'attempt_started':
      return '执行中';
    case 'mailbox_accepted':
      return '已接受';
    case 'deferred':
      return '已保留';
    case 'recovery_required':
      return '需恢复';
    case 'turn_settled':
      return '已结算';
    case 'cancelled':
      return '已取消';
    default:
      return '待处理';
  }
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
