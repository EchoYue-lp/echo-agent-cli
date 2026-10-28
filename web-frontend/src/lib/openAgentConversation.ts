import type { ConversationTarget } from '../generated';
import { GLOBAL_WORKSPACE_ID } from './viewAddress';
import { GLOBAL_WORKSPACE_GENERATION, productDataScope } from './productDataScope';

/** Same navigation admission as the existing sidebar. This never sends, creates or executes. */
export async function openAgentConversation(target: ConversationTarget): Promise<void> {
  // Navigation stores initialize only on a click; merely rendering a tool row has no navigation side effects.
  const [
    { useWorkspaceStore },
    { useConversationStore },
    { useUiStore },
    { useWorkspaceViewStore },
  ] = await Promise.all([
    import('../stores/workspaceStore'),
    import('../stores/conversationStore'),
    import('../stores/uiStore'),
    import('../stores/workspaceViewStore'),
  ]);
  const epoch = useUiStore.getState().beginNavigation();
  const before = useWorkspaceStore.getState();
  const registered = before.workspaces.find((workspace) => workspace.id === target.workspace_id);
  if (target.workspace_id !== GLOBAL_WORKSPACE_ID && !registered)
    throw new Error('目标工作区已不存在');
  if (target.workspace_id !== GLOBAL_WORKSPACE_ID && !target.workspace_generation)
    throw new Error('目标缺少原工作区代际');
  const generation = target.workspace_generation;
  if (
    target.workspace_id === GLOBAL_WORKSPACE_ID &&
    target.workspace_generation !== GLOBAL_WORKSPACE_GENERATION
  )
    throw new Error('全局会话代际已变化');
  if (
    target.workspace_id !== GLOBAL_WORKSPACE_ID &&
    target.workspace_generation &&
    registered?.product_data_generation !== target.workspace_generation
  )
    throw new Error('目标工作区代际已变化');
  const current = before.current?.id ?? GLOBAL_WORKSPACE_ID;
  const sourceEpoch = useConversationStore.getState().newConversationEpoch;
  const transition =
    current === target.workspace_id
      ? null
      : target.workspace_id === GLOBAL_WORKSPACE_ID
        ? before.exit()
        : before.switchTo(target.workspace_id, undefined, generation ?? undefined);
  if (transition) await transition;
  if (useUiStore.getState().navigationEpoch !== epoch) return;
  const draftEpoch = useConversationStore.getState().newConversationEpoch;
  // A fenced workspace switch detaches after its receipt. Additional draft changes supersede the click.
  if (draftEpoch > sourceEpoch + (transition ? 1 : 0)) return;
  const canProject = () =>
    useUiStore.getState().navigationEpoch === epoch &&
    useConversationStore.getState().newConversationEpoch === draftEpoch &&
    useConversationStore.getState().workspaceId === target.workspace_id &&
    (useWorkspaceStore.getState().current?.id ?? GLOBAL_WORKSPACE_ID) === target.workspace_id &&
    (!generation ||
      productDataScope(useWorkspaceStore.getState().current).workspaceGeneration === generation);
  if (!canProject()) return;
  await useConversationStore.getState().loadConversation(target.conversation_id, canProject);
  if (canProject() && useConversationStore.getState().activeId === target.conversation_id)
    useWorkspaceViewStore.getState().openChat();
}
