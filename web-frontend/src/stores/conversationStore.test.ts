// @vitest-environment jsdom
import { beforeEach, afterEach, describe, expect, it, vi } from 'vitest';

vi.hoisted(() => {
  Object.defineProperty(window, 'matchMedia', {
    configurable: true,
    value: () => ({ matches: false }),
  });
});
import { createElement } from 'react';
import { cleanup, fireEvent, render, screen, within, waitFor } from '@testing-library/react';
import { useToolExecutionStore } from './toolExecutionStore';
import { useSubagentRunStore } from './subagentRunStore';
import { useToastStore } from './toastStore';
import { MessageBubble } from '../components/chat/MessageBubble';

const mocks = vi.hoisted(() => ({
  getConversation: vi.fn(),
  listWorkspaces: vi.fn(),
  listToolExecutions: vi.fn(),
  restoreConversation: vi.fn(),
  branchConversation: vi.fn(),
  listConversations: vi.fn(),
  createSideConversation: vi.fn(),
  updateSideConversationModel: vi.fn(),
  retrySideConversation: vi.fn(),
  markSideConversationViewed: vi.fn(),
  updateConversation: vi.fn(),
  deleteConversation: vi.fn(),
  setArchived: vi.fn(),
  resetSession: vi.fn(),
  saveConversation: vi.fn(),
}));

vi.mock('../api/endpoints', () => ({
  sessionApi: { reset: mocks.resetSession },
  workspaceApi: { list: mocks.listWorkspaces },
  conversationApi: {
    list: mocks.listConversations,
    createSide: mocks.createSideConversation,
    updateSideModel: mocks.updateSideConversationModel,
    retrySide: mocks.retrySideConversation,
    markSideViewed: mocks.markSideConversationViewed,
    get: (...args: unknown[]) =>
      mocks.getConversation(...args).then((record: { messages: Record<string, unknown>[] }) => ({
        ...record,
        messages: record.messages.map((message, index) => ({
          created_at: `2026-10-28T00:00:${String(index).padStart(2, '0')}Z`,
          ...message,
        })),
      })),
    save: mocks.saveConversation,
    update: mocks.updateConversation,
    delete: mocks.deleteConversation,
    setArchived: mocks.setArchived,
    restore: mocks.restoreConversation,
    branch: mocks.branchConversation,
  },
  toolExecutionApi: { list: mocks.listToolExecutions },
}));

import type { ChatMessage, SavedMessage } from '../types/api';
import type { ConversationForkRequest } from '../api/endpoints';
import { useWorkspaceStore } from './workspaceStore';
import { useChatStore } from './chatStore';
import { chatMessagesToSaved, restoredMessageId, useConversationStore } from './conversationStore';

function deferred<T>() {
  let resolve: (value: T) => void = () => undefined;
  const promise = new Promise<T>((next) => {
    resolve = next;
  });
  return { promise, resolve };
}

function forkRequest(): ConversationForkRequest {
  return {
    destination: 'current_workspace',
    preparation: {
      request_id: 'fork-request-1',
      workspace_id: 'global',
      workspace_generation: 'global',
      source_id: 'conversation-1',
      user_turn_index: 0,
      prefix_sha256: 'prefix-sha',
      message_count: 2,
      worktree: {
        available: false,
        reason: '非 Git 工作空间',
        repo_root: null,
        head: null,
        branch: 'eko/fork-test',
        path: null,
        project_location: { kind: 'local' as const, path: '/source' },
        ssh_target: null,
        source_connection_generation: null,
        draft_connection_id: null,
      },
    },
  };
}

function forkReceipt() {
  return {
    success: true,
    id: 'branch-1',
    source_id: 'conversation-1',
    source_workspace_id: 'global',
    message_count: 2,
    target_content: 'canonical user prompt',
    request_id: 'fork-request-1',
    destination: 'current_workspace' as const,
    workspace_id: 'global',
    workspace_generation: 'global',
    conversation_created: true,
    imported: true,
  };
}

function sideEntry() {
  return {
    group_id: 'side-group-1',
    workspace_id: 'workspace-1',
    parent_conversation_id: 'conversation-1',
    conversation_id: 'side-1',
    title: 'Explore tests',
    model_id: 'provider:model',
    snapshot_message_count: 2,
    snapshot_last_message_id: 2,
    created_at: '2026-09-15T00:00:00Z',
    updated_at: '2026-09-15T00:00:00Z',
    degraded: false,
    status: 'completed' as const,
    unread_count: 0,
    launch_error: null,
  };
}

beforeEach(() => {
  vi.clearAllMocks();
  useToastStore.getState().clearAll();
  useWorkspaceStore.setState({
    current: null,
    isLoading: false,
    workspaces: [],
  });
  mocks.listToolExecutions.mockResolvedValue([]);
  mocks.restoreConversation.mockResolvedValue(undefined);
  mocks.listConversations.mockResolvedValue([]);
  mocks.listWorkspaces.mockResolvedValue({ workspaces: [], count: 0 });
  mocks.updateConversation.mockResolvedValue(undefined);
  mocks.retrySideConversation.mockResolvedValue({
    creation: { duplicate: true, entry: { group_id: 'side-group-1' } },
    first_turn: { kind: 'started', message_key: 'side-start-1', root_turn_id: 'side-start-1' },
    initial_prompt: 'Retry the original prompt',
    launch_error: null,
  });
  mocks.markSideConversationViewed.mockResolvedValue({ success: true });
  mocks.setArchived.mockResolvedValue({
    success: true,
    conversation_id: 'conversation-1',
    archived: false,
  });
  mocks.branchConversation.mockResolvedValue(forkReceipt());
  mocks.resetSession.mockResolvedValue(undefined);
  mocks.saveConversation.mockImplementation(async (_workspaceId, data) => ({
    kind: 'primary',
    id: data.id,
  }));
  useChatStore.getState().clearMessages();
  useConversationStore.setState({
    workspaceId: 'global',
    activeId: null,
    newConversationEpoch: 0,
    isLoading: false,
    conversations: [],
    sideConversations: [],
    archivedConversationIds: [],
  });
});

describe('conversation clear identity', () => {
  it.each(['other-conversation', 'conversation-a'])(
    'keeps the newly loaded %s when an older reset completes',
    async (nextConversationId) => {
      const reset = deferred<undefined>();
      mocks.resetSession.mockReturnValueOnce(reset.promise);
      useConversationStore.setState({ activeId: 'conversation-a' });
      useChatStore.getState().addUserMessage('old conversation');

      const clearing = useConversationStore.getState().clearCurrent();
      expect(mocks.resetSession).toHaveBeenCalledWith('global', 'conversation-a');
      mocks.getConversation.mockResolvedValueOnce({
        messages: [{ role: 'user', content: 'newly selected conversation' }],
      });
      await useConversationStore.getState().loadConversation(nextConversationId);
      const messages = useChatStore.getState().messages;

      reset.resolve(undefined);
      await clearing;

      expect(useConversationStore.getState().activeId).toBe(nextConversationId);
      expect(useChatStore.getState().messages).toBe(messages);
      expect(messages.map((message) => message.content)).toEqual(['newly selected conversation']);
    }
  );

  it('keeps the newer blank draft and its text when an older reset completes', async () => {
    const reset = deferred<undefined>();
    mocks.resetSession.mockReturnValueOnce(reset.promise);
    useConversationStore.setState({ activeId: 'conversation-a' });
    useChatStore.getState().addUserMessage('old conversation');

    const clearing = useConversationStore.getState().clearCurrent();
    await useConversationStore.getState().startNew();
    useChatStore.getState().addUserMessage('new blank draft');
    const messages = useChatStore.getState().messages;

    reset.resolve(undefined);
    await clearing;

    expect(useConversationStore.getState()).toMatchObject({
      activeId: null,
      newConversationEpoch: 1,
      isLoading: false,
    });
    expect(useChatStore.getState().messages).toBe(messages);
    expect(messages.map((message) => message.content)).toEqual(['new blank draft']);
  });

  it('keeps the new conversation when the older reset fails', async () => {
    const reset = deferred<undefined>();
    mocks.resetSession.mockReturnValueOnce(
      reset.promise.then(() => {
        throw new Error('old reset failed');
      })
    );
    useConversationStore.setState({ activeId: 'conversation-a' });
    const clearing = useConversationStore.getState().clearCurrent();
    mocks.getConversation.mockResolvedValueOnce({
      messages: [{ role: 'user', content: 'new conversation survives failed old reset' }],
    });
    await useConversationStore.getState().loadConversation('other-conversation');
    const messages = useChatStore.getState().messages;

    reset.resolve(undefined);
    await clearing;

    expect(useConversationStore.getState().activeId).toBe('other-conversation');
    expect(useChatStore.getState().messages).toBe(messages);
  });

  it.each([false, true])(
    'clears the original current conversation normally when reset failure is %s',
    async (fails) => {
      if (fails) mocks.resetSession.mockRejectedValueOnce(new Error('reset failed'));
      useConversationStore.setState({ activeId: 'conversation-a' });
      useChatStore.getState().addUserMessage('clear the original conversation');

      await useConversationStore.getState().clearCurrent();

      expect(mocks.resetSession).toHaveBeenCalledWith('global', 'conversation-a');
      expect(useConversationStore.getState()).toMatchObject({ activeId: null, isLoading: false });
      expect(useChatStore.getState().messages).toEqual([]);
    }
  );
});

describe('conversation message identity', () => {
  it('returns the original saved address without selecting it after another conversation opens', async () => {
    const created = deferred<{ kind: string; id: string }>();
    mocks.saveConversation.mockReturnValueOnce(created.promise);
    mocks.getConversation.mockResolvedValueOnce({
      messages: [{ role: 'user', content: 'other conversation' }],
    });
    const saving = useConversationStore
      .getState()
      .saveCurrent([{ id: 'original', role: 'user', content: 'first task', timestamp: 1 }]);
    const requestedId = String(mocks.saveConversation.mock.calls[0]?.[1].id);
    expect(requestedId).not.toBe('undefined');
    await useConversationStore.getState().loadConversation('other-conversation');
    expect(useConversationStore.getState().activeId).toBe('other-conversation');
    created.resolve({ kind: 'primary', id: requestedId });
    expect(await saving).toEqual({ workspace_id: 'global', conversation_id: requestedId });
    expect(useConversationStore.getState().activeId).toBe('other-conversation');
    expect(useChatStore.getState().messages.map((message) => message.content)).toEqual([
      'other conversation',
    ]);
  });

  it('retains a newer blank draft when an old first-conversation save completes', async () => {
    const created = deferred<{ kind: string; id: string }>();
    mocks.saveConversation.mockReturnValueOnce(created.promise);
    const saving = useConversationStore
      .getState()
      .saveCurrent([{ id: 'original', role: 'user', content: 'old draft', timestamp: 1 }]);
    const requestedId = String(mocks.saveConversation.mock.calls[0]?.[1].id);
    await useConversationStore.getState().startNew();
    created.resolve({ kind: 'primary', id: requestedId });
    expect(await saving).toEqual({ workspace_id: 'global', conversation_id: requestedId });
    expect(useConversationStore.getState()).toMatchObject({
      activeId: null,
      newConversationEpoch: 1,
    });
    expect(useChatStore.getState().messages).toEqual([]);
  });

  it('selects its own first conversation and returns its confirmed address normally', async () => {
    const address = await useConversationStore
      .getState()
      .saveCurrent([{ id: 'normal', role: 'user', content: 'normal first draft', timestamp: 1 }]);
    const requestedId = String(mocks.saveConversation.mock.calls[0]?.[1].id);
    expect(address).toEqual({ workspace_id: 'global', conversation_id: requestedId });
    expect(useConversationStore.getState().activeId).toBe(requestedId);
  });

  it('notifies its original sender of the canonical address before selecting the first conversation', async () => {
    const onCreated = vi.fn(() => {
      expect(useConversationStore.getState().activeId).toBeNull();
    });
    const address = await useConversationStore
      .getState()
      .saveCurrent(
        [{ id: 'normal', role: 'user', content: 'normal first draft', timestamp: 1 }],
        onCreated
      );

    expect(onCreated).toHaveBeenCalledExactlyOnceWith(address);
    expect(useConversationStore.getState().activeId).toBe(address?.conversation_id);
  });

  it('returns the successful canonical save even if the identity observer throws', async () => {
    const onCreated = vi.fn(() => {
      throw new Error('view observer failed');
    });
    const address = await useConversationStore
      .getState()
      .saveCurrent(
        [{ id: 'normal', role: 'user', content: 'normal first draft', timestamp: 1 }],
        onCreated
      );
    const requestedId = String(mocks.saveConversation.mock.calls[0]?.[1].id);

    expect(address).toEqual({ workspace_id: 'global', conversation_id: requestedId });
    expect(useConversationStore.getState().activeId).toBe(requestedId);
    expect(mocks.saveConversation).toHaveBeenCalledTimes(1);
  });

  it('persists the assistant message id used by TaskRuntime root_message_id', () => {
    const messages: ChatMessage[] = [
      {
        id: 'assistant-turn-1',
        role: 'assistant',
        content: 'result',
        timestamp: 1,
      },
    ];

    expect(chatMessagesToSaved(messages)[0]?.message_id).toBe('assistant-turn-1');
  });

  it('restores the current canonical persisted message identity', () => {
    const persisted = {
      created_at: '2026-10-28T00:00:00Z',
      message_id: 'assistant-turn-1',
      role: 'assistant',
      content: '',
    };

    expect(restoredMessageId('conversation-1', 2, persisted)).toBe('assistant-turn-1');
  });

  it('does not duplicate pasted or oversized attachment bodies in UI persistence', () => {
    const largeUrl = `data:text/plain;base64,${'A'.repeat(70 * 1024)}`;
    const messages: ChatMessage[] = [
      {
        id: 'user-turn-1',
        role: 'user',
        content: '(附件)',
        timestamp: 1,
        attachments: [
          {
            name: 'pasted-text-1.txt',
            mime_type: 'text/plain',
            url: 'data:text/plain;base64,cGFzdGU=',
            size: 5,
            source: 'paste',
          },
          {
            name: 'large.log',
            mime_type: 'text/plain',
            url: largeUrl,
            size: 70 * 1024,
            source: 'upload',
          },
          {
            name: 'small.txt',
            mime_type: 'text/plain',
            url: 'data:text/plain;base64,c21hbGw=',
            size: 5,
            source: 'upload',
          },
          {
            name: 'screenshot.png',
            mime_type: 'image/png',
            url: 'data:image/png;base64,aW1hZ2U=',
            size: 5,
            source: 'paste',
          },
        ],
      },
    ];

    const attachments = chatMessagesToSaved(messages)[0]?.attachments;
    expect(attachments?.map((attachment) => attachment.url)).toEqual([
      '',
      '',
      'data:text/plain;base64,c21hbGw=',
      'data:image/png;base64,aW1hZ2U=',
    ]);
    expect(attachments?.map((attachment) => attachment.source)).toEqual([
      'paste',
      'upload',
      'upload',
      'paste',
    ]);
  });

  it('clears loading state when a pending conversation load is interrupted by a new chat', async () => {
    const pendingRecord = deferred<{ messages: SavedMessage[] }>();
    mocks.getConversation.mockReturnValueOnce(pendingRecord.promise);

    const load = useConversationStore.getState().loadConversation('conversation-1');
    expect(useConversationStore.getState().isLoading).toBe(true);

    await useConversationStore.getState().startNew();
    pendingRecord.resolve({ messages: [] });
    expect(await load).toBe(false);

    expect(useConversationStore.getState()).toMatchObject({ activeId: null, isLoading: false });
    expect(mocks.restoreConversation).not.toHaveBeenCalled();
  });

  it('keeps an in-flight conversation load alive while the Side status list refreshes', async () => {
    const pendingRecord = deferred<{ messages: SavedMessage[] }>();
    const pendingList = deferred<never[]>();
    mocks.getConversation.mockReturnValueOnce(pendingRecord.promise);
    mocks.listConversations.mockReturnValueOnce(pendingList.promise);
    useConversationStore.setState({ workspaceId: 'workspace-1' });

    const load = useConversationStore.getState().loadConversation('side-1');
    const refresh = useConversationStore.getState().init('workspace-1');
    pendingRecord.resolve({ messages: [] });
    expect(await load).toBe(true);

    expect(useConversationStore.getState()).toMatchObject({
      activeId: 'side-1',
      isLoading: false,
    });
    pendingList.resolve([]);
    await refresh;
  });

  it('opens a blank conversation immediately without resetting a running conversation agent', async () => {
    const pendingUpdate = deferred<undefined>();
    mocks.updateConversation.mockReturnValueOnce(pendingUpdate.promise);
    useConversationStore.setState({ activeId: 'conversation-running' });
    useChatStore.getState().addUserMessage('keep this conversation running');

    const startNew = useConversationStore.getState().startNew();

    expect(useConversationStore.getState()).toMatchObject({
      activeId: null,
      newConversationEpoch: 1,
      isLoading: false,
    });
    expect(useChatStore.getState().messages).toEqual([]);
    expect(mocks.resetSession).not.toHaveBeenCalled();

    pendingUpdate.resolve(undefined);
    await startNew;
  });

  it('activates only the canonical branch returned by the backend', async () => {
    useConversationStore.setState({
      activeId: 'conversation-1',
      conversations: [],
      isLoading: false,
    });

    const result = await useConversationStore.getState().branchCurrent(2);

    expect(mocks.branchConversation).toHaveBeenCalledWith('global', 'conversation-1', 2);
    expect(result).toEqual({ id: 'branch-1', targetContent: 'canonical user prompt' });
    expect(useConversationStore.getState().activeId).toBe('branch-1');
  });

  it('lists an ordinary fork beside its source and opens its committed reply', async () => {
    useConversationStore.setState({ activeId: 'conversation-1' });
    mocks.listConversations.mockResolvedValueOnce(
      ['conversation-1', 'branch-1'].map((id) => ({
        conversation_id: id,
        title: id === 'branch-1' ? 'Original (branch)' : 'Original',
        message_count: 2,
        created_at: '2026-10-08T00:00:00Z',
        updated_at: '2026-10-08T00:00:00Z',
        archived: false,
        side_conversation: null,
      }))
    );
    mocks.getConversation.mockResolvedValueOnce({
      messages: [
        { role: 'user', content: 'original question' },
        { role: 'assistant', content: 'selected reply' },
      ],
    });

    const id = await useConversationStore.getState().forkCurrent(forkRequest());

    expect(id).toMatchObject({ id: 'branch-1', opened: true });
    expect(mocks.branchConversation).toHaveBeenCalledWith(
      'global',
      'conversation-1',
      0,
      true,
      forkRequest()
    );
    expect(useConversationStore.getState().conversations.map((item) => item.id)).toEqual([
      'conversation-1',
      'branch-1',
    ]);
    expect(useConversationStore.getState().sideConversations).toEqual([]);
    expect(useConversationStore.getState().activeId).toBe('branch-1');
    expect(useChatStore.getState().messages.map((item) => item.content)).toEqual([
      'original question',
      'selected reply',
    ]);
    expect(mocks.restoreConversation).toHaveBeenCalledWith('global', 'branch-1');
  });

  it('does not replace a newer conversation selection when Fork completes late', async () => {
    const response = deferred<ReturnType<typeof forkReceipt>>();
    mocks.branchConversation.mockReturnValueOnce(response.promise);
    useConversationStore.setState({ activeId: 'conversation-1' });
    const forking = useConversationStore.getState().forkCurrent(forkRequest());
    useConversationStore.setState({ activeId: 'conversation-2' });
    response.resolve(forkReceipt());
    await forking;
    expect(useConversationStore.getState().activeId).toBe('conversation-2');
    expect(mocks.getConversation).not.toHaveBeenCalled();
  });

  it('keeps a cancelled observer or newer page from opening a completed Fork', async () => {
    useConversationStore.setState({ activeId: 'conversation-1' });
    const response = deferred<ReturnType<typeof forkReceipt>>();
    mocks.branchConversation.mockReturnValueOnce(response.promise);
    let currentPage = true;
    const forking = useConversationStore.getState().forkCurrent(forkRequest(), () => currentPage);
    currentPage = false;
    response.resolve(forkReceipt());
    expect(await forking).toMatchObject({ id: 'branch-1', opened: false });
    expect(mocks.getConversation).not.toHaveBeenCalled();
    expect(useConversationStore.getState().activeId).toBe('conversation-1');
    await expect(
      useConversationStore.getState().forkCurrent(forkRequest(), () => false)
    ).resolves.toMatchObject({ opened: false });
    expect(mocks.getConversation).not.toHaveBeenCalled();
  });

  it('retains partial workspace facts without opening or deleting them', async () => {
    useConversationStore.setState({ activeId: 'conversation-1' });
    const workspace = {
      id: 'new-workspace',
      name: '保留的空间',
      root: '/product/new',
      created_at: '2026-10-27T00:00:00Z',
      last_active: '2026-10-27T00:00:00Z',
      kind: { type: 'code' },
      metadata: { tags: [], project_location: { kind: 'local' as const, path: '/product/new' } },
      product_data_generation: 'new-generation',
    };
    mocks.listWorkspaces.mockResolvedValueOnce({ workspaces: [workspace], count: 1 });
    mocks.branchConversation.mockResolvedValueOnce({
      ...forkReceipt(),
      success: false,
      imported: false,
      id: null,
      workspace,
      workspace_id: workspace.id,
      workspace_generation: workspace.product_data_generation,
      error: '原文迁移失败',
    });
    const result = await useConversationStore.getState().forkCurrent(forkRequest());
    expect(result).toMatchObject({
      opened: false,
      workspace,
      error: '原文迁移失败',
    });
    expect(useWorkspaceStore.getState().workspaces).toContainEqual(workspace);
    expect(useConversationStore.getState().activeId).toBe('conversation-1');
    expect(mocks.getConversation).not.toHaveBeenCalled();
  });

  it('does not replace a reopened target generation with a late fork resource receipt', async () => {
    useConversationStore.setState({ activeId: 'conversation-1' });
    const workspace = {
      id: 'reopened-target',
      name: '当前新代际',
      root: '/product/current',
      created_at: '2026-10-27T00:00:00Z',
      last_active: '2026-10-27T00:00:00Z',
      kind: { type: 'code' },
      metadata: {
        tags: [],
        project_location: { kind: 'local' as const, path: '/product/current' },
      },
      product_data_generation: 'current-generation',
    };
    useWorkspaceStore.setState({ workspaces: [workspace] });
    const stale = {
      ...workspace,
      name: '旧回执',
      product_data_generation: 'retired-generation',
    };
    mocks.branchConversation.mockResolvedValueOnce({
      ...forkReceipt(),
      success: false,
      imported: false,
      id: null,
      workspace: stale,
      workspace_id: stale.id,
      workspace_generation: stale.product_data_generation,
      error: '历史复制未完成',
    });
    const receipt = await useConversationStore.getState().forkCurrent(forkRequest());
    expect(receipt).toMatchObject({ opened: false, workspace: stale });
    expect(useWorkspaceStore.getState().workspaces).toEqual([workspace]);
    expect(mocks.getConversation).not.toHaveBeenCalled();
  });

  it('does not switch to a successor target generation after a successful old fork receipt', async () => {
    const switchTo = useWorkspaceStore.getState().switchTo;
    const switching = vi.fn();
    const workspace = {
      id: 'reopened-success',
      name: '真实新目标',
      root: '/product/current',
      created_at: '2026-10-27T00:00:00Z',
      last_active: '2026-10-27T00:00:00Z',
      kind: { type: 'code' },
      metadata: {
        tags: [],
        project_location: { kind: 'local' as const, path: '/product/current' },
      },
      product_data_generation: 'current-generation',
    };
    const stale = { ...workspace, product_data_generation: 'old-generation' };
    useConversationStore.setState({ activeId: 'conversation-1' });
    useWorkspaceStore.setState({ workspaces: [workspace], switchTo: switching });
    const request = forkRequest();
    request.destination = 'new_worktree';
    mocks.branchConversation.mockResolvedValueOnce({
      ...forkReceipt(),
      destination: 'new_worktree',
      workspace: stale,
      workspace_id: stale.id,
      workspace_generation: stale.product_data_generation,
    });
    try {
      const receipt = await useConversationStore.getState().forkCurrent(request);
      expect(receipt).toMatchObject({ success: true, opened: false });
      expect(switching).not.toHaveBeenCalled();
      expect(useWorkspaceStore.getState().workspaces).toEqual([workspace]);
      expect(mocks.getConversation).not.toHaveBeenCalled();
    } finally {
      useWorkspaceStore.setState({ switchTo });
    }
  });

  it('does not resurrect a target deleted from the canonical registry after a fork receipt', async () => {
    useConversationStore.setState({ activeId: 'conversation-1' });
    const workspace = {
      id: 'deleted-target',
      name: '旧资源',
      root: '/product/deleted',
      created_at: '',
      last_active: '',
      kind: { type: 'code' },
      metadata: {
        tags: [],
        project_location: { kind: 'local' as const, path: '/product/deleted' },
      },
      product_data_generation: 'old-generation',
    };
    mocks.branchConversation.mockResolvedValueOnce({
      ...forkReceipt(),
      workspace,
      workspace_id: workspace.id,
      workspace_generation: workspace.product_data_generation,
    });
    mocks.listWorkspaces.mockResolvedValueOnce({ workspaces: [], count: 0 });
    expect(await useConversationStore.getState().forkCurrent(forkRequest())).toMatchObject({
      opened: false,
      workspace,
    });
    expect(useWorkspaceStore.getState().workspaces).toEqual([]);
    expect(mocks.getConversation).not.toHaveBeenCalled();
  });

  it('rejects a mismatched source generation before any creation', async () => {
    useConversationStore.setState({ activeId: 'conversation-1' });
    const request = forkRequest();
    request.preparation.workspace_generation = 'stale-generation';
    await expect(useConversationStore.getState().forkCurrent(request)).rejects.toThrow(
      '来源已变化'
    );
    expect(mocks.branchConversation).not.toHaveBeenCalled();
  });

  it('opens the exact new workspace and imported idle conversation through the original switch', async () => {
    const switchTo = useWorkspaceStore.getState().switchTo;
    const workspace = {
      id: 'new-workspace',
      name: '独立空间',
      root: '/product/new',
      created_at: '2026-10-27T00:00:00Z',
      last_active: '2026-10-27T00:00:00Z',
      kind: { type: 'code' },
      metadata: { tags: [], project_location: { kind: 'local' as const, path: '/product/new' } },
      product_data_generation: 'target-generation',
    };
    const request = forkRequest();
    request.destination = 'new_worktree';
    useConversationStore.setState({ activeId: 'conversation-1' });
    mocks.listWorkspaces.mockResolvedValueOnce({ workspaces: [workspace], count: 1 });
    mocks.branchConversation.mockResolvedValueOnce({
      ...forkReceipt(),
      destination: 'new_worktree',
      workspace,
      workspace_id: workspace.id,
      workspace_generation: workspace.product_data_generation,
    });
    mocks.getConversation.mockResolvedValueOnce({
      messages: [
        { role: 'user', content: '源问题' },
        { role: 'assistant', content: '完整回复' },
      ],
    });
    const switching = vi.fn(async (id: string) => {
      useConversationStore.getState().detachForWorkspace('global');
      useWorkspaceStore.setState({ current: workspace });
      await useConversationStore.getState().init(id);
    });
    useWorkspaceStore.setState({ switchTo: switching });
    try {
      const receipt = await useConversationStore.getState().forkCurrent(request);
      expect(receipt).toMatchObject({
        opened: true,
        workspace_id: workspace.id,
        id: 'branch-1',
      });
      expect(switching).toHaveBeenCalledExactlyOnceWith(
        workspace.id,
        undefined,
        workspace.product_data_generation
      );
      expect(mocks.getConversation).toHaveBeenCalledWith(workspace.id, 'branch-1');
      expect(mocks.restoreConversation).toHaveBeenCalledWith(workspace.id, 'branch-1');
      expect(useConversationStore.getState().activeId).toBe('branch-1');
    } finally {
      useWorkspaceStore.setState({ switchTo });
    }
  });

  it('keeps a created target when switching fails and never reports it as uncreated', async () => {
    const switchTo = useWorkspaceStore.getState().switchTo;
    const workspace = {
      id: 'new-workspace',
      name: '尚未打开',
      root: '/product/new',
      created_at: '2026-10-27T00:00:00Z',
      last_active: '2026-10-27T00:00:00Z',
      kind: { type: 'code' },
      metadata: { tags: [], project_location: { kind: 'local' as const, path: '/product/new' } },
      product_data_generation: 'target-generation',
    };
    const request = forkRequest();
    request.destination = 'new_worktree';
    useConversationStore.setState({ activeId: 'conversation-1' });
    mocks.listWorkspaces.mockResolvedValueOnce({ workspaces: [workspace], count: 1 });
    mocks.branchConversation.mockResolvedValueOnce({
      ...forkReceipt(),
      destination: 'new_worktree',
      workspace,
      workspace_id: workspace.id,
      workspace_generation: workspace.product_data_generation,
    });
    useWorkspaceStore.setState({
      switchTo: vi.fn().mockRejectedValue(new Error('真实目标连接已断开')),
    });
    try {
      expect(await useConversationStore.getState().forkCurrent(request)).toMatchObject({
        success: true,
        imported: true,
        opened: false,
        workspace,
        open_error: '真实目标连接已断开',
      });
      expect(useWorkspaceStore.getState().workspaces).toContainEqual(workspace);
      expect(mocks.getConversation).not.toHaveBeenCalled();
    } finally {
      useWorkspaceStore.setState({ switchTo });
    }
  });

  it('does not publish a fork whose observer closes during history hydration', async () => {
    useConversationStore.setState({ activeId: 'conversation-1' });
    const sourceMessages: ChatMessage[] = [
      {
        id: 'original',
        role: 'assistant',
        content: '保留原历史',
        timestamp: 1,
      },
    ];
    useChatStore.getState().replaceMessages(sourceMessages);
    mocks.getConversation.mockResolvedValueOnce({
      messages: [
        { role: 'user', content: '源问题' },
        { role: 'assistant', content: '复制回复' },
      ],
    });
    const restoration = deferred<void>();
    mocks.restoreConversation.mockReturnValueOnce(restoration.promise);
    let observing = true;
    const forking = useConversationStore.getState().forkCurrent(forkRequest(), () => observing);
    await waitFor(() =>
      expect(mocks.restoreConversation).toHaveBeenCalledWith('global', 'branch-1')
    );
    observing = false;
    restoration.resolve(undefined);
    expect(await forking).toMatchObject({ opened: false });
    expect(useConversationStore.getState().activeId).toBe('conversation-1');
    expect(useConversationStore.getState().isLoading).toBe(false);
    expect(useChatStore.getState().messages).toEqual(sourceMessages);
  });

  it('preserves the source selection and transcript when Fork fails', async () => {
    mocks.branchConversation.mockRejectedValueOnce(new Error('source turn is active'));
    useConversationStore.setState({ activeId: 'conversation-1' });
    useChatStore
      .getState()
      .replaceMessages([{ id: 'reply', role: 'assistant', content: 'original', timestamp: 1 }]);
    await expect(useConversationStore.getState().forkCurrent(forkRequest())).rejects.toThrow(
      'source turn is active'
    );
    expect(useConversationStore.getState().activeId).toBe('conversation-1');
    expect(useChatStore.getState().messages.at(0)?.content).toBe('original');
  });

  it('creates a Side Conversation under the active primary and opens the canonical child', async () => {
    mocks.createSideConversation.mockResolvedValueOnce({
      creation: {
        duplicate: false,
        entry: {
          group_id: 'side-group-1',
          workspace_id: 'workspace-1',
          parent_conversation_id: 'conversation-1',
          conversation_id: 'side-1',
          title: 'Explore tests',
          model_id: 'provider:model',
          snapshot_message_count: 2,
          snapshot_last_message_id: 2,
          created_at: '2026-09-15T00:00:00Z',
          updated_at: '2026-09-15T00:00:00Z',
          degraded: false,
          status: 'queued',
          unread_count: 0,
          launch_error: null,
        },
      },
      first_turn: { kind: 'started', message_key: 'side-start-1', root_turn_id: 'side-start-1' },
      initial_prompt: 'Explore the test strategy',
      launch_error: null,
    });
    mocks.getConversation.mockResolvedValueOnce({ messages: [] });
    useConversationStore.setState({
      workspaceId: 'workspace-1',
      activeId: 'conversation-1',
      conversations: [],
      sideConversations: [],
    });

    let acknowledgement: unknown;
    const onCreated = vi.fn((canProject: () => boolean) => {
      acknowledgement = {
        canProject: canProject(),
        activeId: useConversationStore.getState().activeId,
      };
    });
    const created = await useConversationStore.getState().createSideConversation({
      requestId: 'request-1',
      prompt: 'Explore the test strategy',
      title: 'Explore tests',
      modelId: 'provider:model',
      onCreated,
    });

    expect(onCreated).toHaveBeenCalledOnce();
    expect(acknowledgement).toEqual({ canProject: true, activeId: 'conversation-1' });
    expect(mocks.createSideConversation).toHaveBeenCalledWith(
      expect.objectContaining({
        workspace_id: 'workspace-1',
        parent_conversation_id: 'conversation-1',
        request_id: 'request-1',
        prompt: 'Explore the test strategy',
        title: 'Explore tests',
        model_id: 'provider:model',
      })
    );
    expect(created.conversation_id).toBe('side-1');
    expect(useConversationStore.getState().activeId).toBe('side-1');
    expect(useChatStore.getState().messages).toEqual([
      expect.objectContaining({
        id: 'side-start-1',
        role: 'user',
        content: 'Explore the test strategy',
      }),
    ]);
  });

  it('does not duplicate a fast completed initial prompt restored with its stable identity', async () => {
    mocks.createSideConversation.mockResolvedValueOnce({
      creation: { duplicate: false, entry: sideEntry() },
      first_turn: {
        kind: 'completed',
        message_key: 'side-start:side-group-1',
        root_turn_id: 'side-start:side-group-1',
      },
      initial_prompt: 'Explore the test strategy',
      launch_error: null,
    });
    mocks.getConversation.mockResolvedValueOnce({
      messages: [
        {
          message_id: 'side-start:side-group-1',
          role: 'user',
          content: 'Explore the test strategy',
        },
        { role: 'assistant', content: 'Finished quickly' },
      ],
    });
    useConversationStore.setState({
      workspaceId: 'workspace-1',
      activeId: 'conversation-1',
      conversations: [],
      sideConversations: [],
    });

    await useConversationStore.getState().createSideConversation({
      requestId: 'request-fast',
      prompt: 'Explore the test strategy',
    });

    expect(
      useChatStore
        .getState()
        .messages.filter((message) => message.content === 'Explore the test strategy')
    ).toHaveLength(1);
  });

  it('does not duplicate the canonical prompt when a lost create response is retried', async () => {
    mocks.createSideConversation.mockResolvedValueOnce({
      creation: { duplicate: true, entry: sideEntry() },
      first_turn: {
        kind: 'completed',
        message_key: 'side-start:side-group-1',
        root_turn_id: 'side-start:side-group-1',
      },
      initial_prompt: 'Explore the test strategy',
      launch_error: null,
    });
    mocks.getConversation.mockResolvedValueOnce({
      messages: [
        {
          message_id: 'side-start:side-group-1',
          role: 'user',
          content: 'Explore the test strategy',
        },
        { role: 'assistant', content: 'Recovered result' },
      ],
    });
    useConversationStore.setState({
      workspaceId: 'workspace-1',
      activeId: 'conversation-1',
      conversations: [],
      sideConversations: [],
    });

    await useConversationStore.getState().createSideConversation({
      requestId: 'request-lost-response',
      prompt: 'Explore the test strategy',
    });

    expect(mocks.createSideConversation).toHaveBeenCalledTimes(1);
    expect(
      useChatStore
        .getState()
        .messages.filter((message) => message.content === 'Explore the test strategy')
    ).toHaveLength(1);
  });

  it('rejects nested Side Conversation creation before calling the backend', async () => {
    useConversationStore.setState({
      workspaceId: 'workspace-1',
      activeId: 'side-1',
      sideConversations: [
        {
          group_id: 'side-group-1',
          workspace_id: 'workspace-1',
          parent_conversation_id: 'conversation-1',
          conversation_id: 'side-1',
          title: 'Existing side',
          model_id: null,
          snapshot_message_count: 0,
          snapshot_last_message_id: null,
          created_at: '2026-09-15T00:00:00Z',
          updated_at: '2026-09-15T00:00:00Z',
          degraded: false,
          status: 'idle',
          unread_count: 0,
          launch_error: null,
        },
      ],
    });

    await expect(
      useConversationStore
        .getState()
        .createSideConversation({ requestId: 'nested-request', prompt: 'nested' })
    ).rejects.toThrow('cannot create nested');
    expect(mocks.createSideConversation).not.toHaveBeenCalled();
  });

  it('keeps a late Side creation receipt from opening over a different selected conversation', async () => {
    const onCreated = vi.fn();
    const response = deferred<{
      creation: { duplicate: boolean; entry: ReturnType<typeof sideEntry> };
      first_turn: null;
      initial_prompt: string;
      launch_error: null;
    }>();
    mocks.createSideConversation.mockReturnValueOnce(response.promise);
    useConversationStore.setState({ workspaceId: 'workspace-1', activeId: 'conversation-1' });
    const creating = useConversationStore.getState().createSideConversation({
      requestId: 'request-late-side',
      prompt: '保留原支线意图',
      onCreated,
    });
    useConversationStore.setState({ activeId: 'another-conversation' });
    useChatStore.getState().addUserMessage('新页面内容');
    response.resolve({
      creation: { duplicate: false, entry: sideEntry() },
      first_turn: null,
      initial_prompt: '保留原支线意图',
      launch_error: null,
    });
    await expect(creating).resolves.toMatchObject({ conversation_id: 'side-1' });
    expect(useConversationStore.getState().activeId).toBe('another-conversation');
    expect(useChatStore.getState().messages.at(-1)?.content).toBe('新页面内容');
    expect(mocks.getConversation).not.toHaveBeenCalled();
    expect(onCreated).not.toHaveBeenCalled();
  });

  it('reloads the active Side Conversation after changing its model', async () => {
    mocks.updateSideConversationModel.mockResolvedValueOnce({ success: true });
    mocks.getConversation.mockResolvedValueOnce({ messages: [] });
    useConversationStore.setState({
      workspaceId: 'workspace-1',
      activeId: 'side-1',
      sideConversations: [
        {
          group_id: 'side-group-1',
          workspace_id: 'workspace-1',
          parent_conversation_id: 'conversation-1',
          conversation_id: 'side-1',
          title: 'Existing side',
          model_id: 'provider:old',
          snapshot_message_count: 0,
          snapshot_last_message_id: null,
          created_at: '2026-09-15T00:00:00Z',
          updated_at: '2026-09-15T00:00:00Z',
          degraded: false,
          status: 'idle',
          unread_count: 0,
          launch_error: null,
        },
      ],
    });

    await useConversationStore.getState().updateSideConversationModel('side-1', 'provider:new');

    expect(mocks.updateSideConversationModel).toHaveBeenCalledWith(
      'workspace-1',
      'side-1',
      'provider:new'
    );
    expect(mocks.getConversation).toHaveBeenCalledWith('workspace-1', 'side-1');
    expect(mocks.restoreConversation).toHaveBeenCalledWith('workspace-1', 'side-1');
    expect(useConversationStore.getState().sideConversations[0]?.model_id).toBe('provider:new');
  });

  it('renames a Side Conversation through the shared conversation update contract', async () => {
    useConversationStore.setState({
      workspaceId: 'workspace-1',
      conversations: [
        {
          id: 'side-1',
          title: 'Old title',
          lastMessage: '',
          messageCount: 1,
          createdAt: 1,
          updatedAt: 2,
          workspaceId: 'workspace-1',
        },
      ],
      sideConversations: [
        {
          group_id: 'side-group-1',
          workspace_id: 'workspace-1',
          parent_conversation_id: 'conversation-1',
          conversation_id: 'side-1',
          title: 'Old title',
          model_id: null,
          snapshot_message_count: 1,
          snapshot_last_message_id: 1,
          created_at: '2026-09-15T00:00:00Z',
          updated_at: '2026-09-15T00:00:00Z',
          degraded: false,
          status: 'idle',
          unread_count: 0,
          launch_error: null,
        },
      ],
    });

    await useConversationStore.getState().renameConversation('side-1', 'New title');

    expect(mocks.updateConversation).toHaveBeenCalledWith('workspace-1', 'side-1', {
      title: 'New title',
    });
    expect(useConversationStore.getState().conversations[0]?.title).toBe('New title');
    expect(useConversationStore.getState().sideConversations[0]?.title).toBe('New title');
  });

  it('retries a failed initial prompt and reopens the Side Conversation', async () => {
    mocks.getConversation.mockResolvedValueOnce({ messages: [] });
    useConversationStore.setState({
      workspaceId: 'workspace-1',
      activeId: 'conversation-1',
      sideConversations: [
        {
          group_id: 'side-group-1',
          workspace_id: 'workspace-1',
          parent_conversation_id: 'conversation-1',
          conversation_id: 'side-1',
          title: 'Failed side',
          model_id: null,
          snapshot_message_count: 1,
          snapshot_last_message_id: 1,
          created_at: '2026-09-15T00:00:00Z',
          updated_at: '2026-09-15T00:00:00Z',
          degraded: false,
          status: 'failed',
          unread_count: 0,
          launch_error: 'model unavailable',
        },
      ],
    });

    await useConversationStore.getState().retrySideConversation('side-1');

    expect(mocks.retrySideConversation).toHaveBeenCalledWith('workspace-1', 'side-1');
    expect(mocks.getConversation).toHaveBeenCalledWith('workspace-1', 'side-1');
    expect(useConversationStore.getState().activeId).toBe('side-1');
  });

  it('publishes a running hint so lifecycle polling starts for a later Side turn', () => {
    useConversationStore.setState({
      sideConversations: [
        {
          group_id: 'side-group-1',
          workspace_id: 'workspace-1',
          parent_conversation_id: 'conversation-1',
          conversation_id: 'side-1',
          title: 'Side',
          model_id: null,
          snapshot_message_count: 1,
          snapshot_last_message_id: 1,
          created_at: '2026-09-15T00:00:00Z',
          updated_at: '2026-09-15T00:00:00Z',
          degraded: false,
          status: 'completed',
          unread_count: 0,
          launch_error: null,
        },
      ],
    });

    useConversationStore.getState().noteSideConversationTurnStarted('side-1');

    expect(useConversationStore.getState().sideConversations[0]?.status).toBe('running');
  });

  it('persists the viewed marker when the active Side receives a newer unread projection', async () => {
    useConversationStore.setState({ workspaceId: 'workspace-1', activeId: 'side-1' });
    mocks.listConversations.mockResolvedValueOnce([
      {
        id: 1,
        conversation_id: 'side-1',
        title: 'Visible side',
        message_count: 2,
        created_at: '2026-09-15T00:00:00Z',
        updated_at: '2026-09-15T00:01:00Z',
        side_conversation: {
          group_id: 'side-group-1',
          workspace_id: 'workspace-1',
          parent_conversation_id: 'conversation-1',
          conversation_id: 'side-1',
          title: 'Visible side',
          model_id: null,
          snapshot_message_count: 1,
          snapshot_last_message_id: 1,
          created_at: '2026-09-15T00:00:00Z',
          updated_at: '2026-09-15T00:01:00Z',
          degraded: false,
          status: 'completed' as const,
          unread_count: 1,
          launch_error: null,
        },
      },
    ]);

    await useConversationStore.getState().init('workspace-1');

    expect(useConversationStore.getState().sideConversations[0]?.unread_count).toBe(0);
    expect(mocks.markSideConversationViewed).toHaveBeenCalledWith('workspace-1', 'side-1');
  });

  it('archives and restores conversations through the workspace-scoped application API', async () => {
    useConversationStore.setState({
      workspaceId: 'workspace-1',
      conversations: [
        {
          id: 'conversation-1',
          title: 'First',
          lastMessage: '',
          messageCount: 2,
          createdAt: 1,
          updatedAt: 2,
          workspaceId: 'workspace-1',
        },
      ],
    });

    await useConversationStore.getState().archiveConversation('conversation-1');
    expect(useConversationStore.getState().conversations[0]?.archived).toBe(true);
    expect(mocks.setArchived).toHaveBeenCalledWith('workspace-1', 'conversation-1', true);

    await useConversationStore.getState().restoreConversation('conversation-1');
    expect(useConversationStore.getState().conversations[0]?.archived).toBe(false);
    expect(mocks.setArchived).toHaveBeenLastCalledWith('workspace-1', 'conversation-1', false);
  });

  it('deletes through the backend before removing the local conversation projection', async () => {
    mocks.deleteConversation.mockResolvedValueOnce({ cleanup_pending: false });
    useConversationStore.setState({
      workspaceId: 'workspace-1',
      activeId: null,
      conversations: [
        {
          id: 'conversation-1',
          title: 'First',
          lastMessage: '',
          messageCount: 2,
          createdAt: 1,
          updatedAt: 2,
          workspaceId: 'workspace-1',
        },
      ],
    });

    await useConversationStore.getState().deleteConversation('conversation-1');

    expect(mocks.deleteConversation).toHaveBeenCalledWith('workspace-1', 'conversation-1');
    expect(useConversationStore.getState().conversations).toEqual([]);
  });

  it('clears an active child when deleting its primary conversation', async () => {
    mocks.deleteConversation.mockResolvedValueOnce({ cleanup_pending: false });
    useConversationStore.setState({
      workspaceId: 'workspace-1',
      activeId: 'side-1',
      conversations: [
        {
          id: 'conversation-1',
          title: 'Primary',
          lastMessage: '',
          messageCount: 2,
          createdAt: 1,
          updatedAt: 2,
          workspaceId: 'workspace-1',
        },
        {
          id: 'side-1',
          title: 'Side',
          lastMessage: '',
          messageCount: 1,
          createdAt: 1,
          updatedAt: 2,
          workspaceId: 'workspace-1',
        },
      ],
      sideConversations: [
        {
          group_id: 'side-group-1',
          workspace_id: 'workspace-1',
          parent_conversation_id: 'conversation-1',
          conversation_id: 'side-1',
          title: 'Side',
          model_id: null,
          snapshot_message_count: 1,
          snapshot_last_message_id: 1,
          created_at: '2026-09-15T00:00:00Z',
          updated_at: '2026-09-15T00:00:00Z',
          degraded: false,
          status: 'idle',
          unread_count: 0,
          launch_error: null,
        },
      ],
    });

    await useConversationStore.getState().deleteConversation('conversation-1');

    expect(useConversationStore.getState()).toMatchObject({
      activeId: null,
      conversations: [],
      sideConversations: [],
    });
  });
});

afterEach(cleanup);

function canonicalHistoryCall(overrides: Record<string, unknown> = {}) {
  return {
    message_id: 'canonical-assistant',
    role: 'assistant',
    content: null,
    tool_calls: [
      {
        id: 'canonical-call',
        type: 'function',
        function: { name: 'shell', arguments: '{"command":"echo 中文 🧭"}' },
      },
    ],
    ...overrides,
  };
}
function canonicalHistoryResult(content = '[Error] 原记录正文') {
  return {
    role: 'tool',
    content,
    tool_result: JSON.stringify({ tool_call_id: 'canonical-call', name: 'shell' }),
  };
}
function canonicalHistorySummary(overrides: Record<string, unknown> = {}) {
  return {
    id: 'known-detail',
    workspace_id: 'global',
    conversation_id: 'canonical-history',
    run_id: 'known-run',
    owner: { kind: 'chat', message_id: 'canonical-assistant' },
    call_id: 'canonical-call',
    name: 'shell',
    args_preview: 'echo',
    status: 'failed',
    started_at: 100,
    finished_at: 200,
    duration_ms: 100,
    detail_ref: 'known-detail',
    ...overrides,
  };
}

describe('canonical tool transcript recovery', () => {
  it('reuses an exact existing display reference across duplicate call ids without granting status or output ownership', async () => {
    mocks.getConversation.mockResolvedValue({
      messages: [
        canonicalHistoryCall({ execution_steps: [{ type: 'tool', call_id: 'known-detail' }] }),
        canonicalHistoryCall({ message_id: 'other-canonical-message' }),
        canonicalHistoryResult('重复call不能借给任何一条原文'),
      ],
    });
    mocks.listToolExecutions.mockResolvedValue([canonicalHistorySummary({ status: 'succeeded' })]);
    await useConversationStore.getState().loadConversation('canonical-history');
    const first = useChatStore.getState().messages[0];
    expect(first?.executionSteps).toEqual([{ type: 'tool', callId: 'known-detail' }]);
    expect(first?.transcriptTools).toHaveLength(1);
    expect(first?.transcriptTools?.[0]).toMatchObject({
      id: 'known-detail',
      identityAmbiguous: true,
    });
    expect(first?.transcriptTools?.[0]?.recordedOutput).toBeUndefined();
    if (!first) throw new Error('missing first canonical message');
    render(createElement(MessageBubble, { message: first }));
    fireEvent.click(screen.getByRole('button', { name: '展开执行过程' }));
    expect(screen.getByText('1 个工具 · 1 个状态未恢复')).toBeDefined();
    fireEvent.click(screen.getByRole('button', { name: '展开工具执行过程' }));
    expect(screen.getByRole('button', { name: /shell.*状态未恢复/ })).toBeDefined();
    expect(screen.queryByText('success', { exact: true })).toBeNull();
    expect(useToolExecutionStore.getState().tools['known-detail']?.status).toBe('succeeded');
  });

  it('preserves a legacy opaque execution reference only with its exact persisted message owner', async () => {
    mocks.getConversation.mockResolvedValue({
      messages: [
        {
          message_id: 'canonical-assistant',
          role: 'assistant',
          content: '',
          execution_steps: [{ type: 'tool', call_id: 'known-detail' }],
        },
      ],
    });
    mocks.listToolExecutions.mockResolvedValue([canonicalHistorySummary()]);
    await useConversationStore.getState().loadConversation('canonical-history');
    const message = useChatStore.getState().messages[0];
    expect(message?.executionSteps).toEqual([{ type: 'tool', callId: 'known-detail' }]);
    if (!message) throw new Error('missing history');
    render(createElement(MessageBubble, { message }));
    fireEvent.click(screen.getByRole('button', { name: '展开执行过程' }));
    expect(screen.getByText(/1 个失败/)).toBeDefined();
    expect(screen.queryByText('历史执行状态未恢复')).toBeNull();
  });

  it('still reports the current restore failure and keeps its loaded transcript read-only', async () => {
    mocks.getConversation.mockResolvedValue({
      messages: [{ role: 'assistant', content: '可读历史' }],
    });
    mocks.restoreConversation.mockRejectedValueOnce(new Error('current context restore failed'));
    expect(await useConversationStore.getState().loadConversation('current-failed-restore')).toBe(
      true
    );
    expect(useConversationStore.getState().activeId).toBe('current-failed-restore');
    expect(useChatStore.getState().isHistoryView).toBe(true);
    expect(useChatStore.getState().messages.map((message) => message.content)).toEqual([
      '可读历史',
    ]);
    expect(useToastStore.getState().toasts).toEqual([
      expect.objectContaining({
        type: 'error',
        message: '会话上下文恢复失败，已切换为只读历史视图',
      }),
    ]);
  });

  it('keeps an ownerless canonical call with an existing detail reference to one unknown row', async () => {
    mocks.getConversation.mockResolvedValue({
      messages: [
        canonicalHistoryCall({
          message_id: undefined,
          execution_steps: [{ type: 'tool', call_id: 'known-detail' }],
        }),
        canonicalHistoryResult(),
      ],
    });
    mocks.listToolExecutions.mockResolvedValue([canonicalHistorySummary({ status: 'succeeded' })]);
    await useConversationStore.getState().loadConversation('canonical-history');
    const message = useChatStore.getState().messages[0];
    expect(message?.executionSteps).toEqual([{ type: 'tool', callId: 'known-detail' }]);
    if (!message) throw new Error('missing history');
    render(createElement(MessageBubble, { message }));
    fireEvent.click(screen.getByRole('button', { name: '展开执行过程' }));
    expect(screen.getByText('1 个工具 · 1 个状态未恢复')).toBeDefined();
    fireEvent.click(screen.getByRole('button', { name: '展开工具执行过程' }));
    expect(screen.getByRole('button', { name: /shell.*状态未恢复/ })).toBeDefined();
    expect(screen.queryByText('success', { exact: true })).toBeNull();
  });

  it('does not borrow a terminal status through a bare legacy execution key without an owner', async () => {
    mocks.getConversation.mockResolvedValue({
      messages: [
        {
          role: 'assistant',
          content: '',
          execution_steps: [{ type: 'tool', call_id: 'known-detail' }],
        },
      ],
    });
    mocks.listToolExecutions.mockResolvedValue([canonicalHistorySummary({ status: 'succeeded' })]);
    await useConversationStore.getState().loadConversation('canonical-history');
    const message = useChatStore.getState().messages[0];
    if (!message) throw new Error('missing history');
    render(createElement(MessageBubble, { message }));
    fireEvent.click(screen.getByRole('button', { name: '展开执行过程' }));
    expect(screen.getByText('1 个工具 · 1 个状态未恢复')).toBeDefined();
  });

  it('does not let a failed old inner restore turn a newer loaded conversation into history-only mode', async () => {
    let rejectRestore: (error: Error) => void = () => undefined;
    const restoreA = new Promise<never>((_resolve, reject) => {
      rejectRestore = reject;
    });
    mocks.getConversation
      .mockResolvedValueOnce({ messages: [{ role: 'assistant', content: 'A正文' }] })
      .mockResolvedValueOnce({ messages: [{ role: 'assistant', content: 'B正文' }] });
    mocks.restoreConversation.mockReturnValueOnce(restoreA).mockResolvedValueOnce(undefined);
    const loadingA = useConversationStore.getState().loadConversation('conversation-A');
    await waitFor(() =>
      expect(mocks.restoreConversation).toHaveBeenCalledWith('global', 'conversation-A')
    );
    await useConversationStore.getState().loadConversation('conversation-B');
    expect(useChatStore.getState().isHistoryView).toBe(false);
    rejectRestore(new Error('late inner A restore error'));
    expect(await loadingA).toBe(false);
    expect(useConversationStore.getState().activeId).toBe('conversation-B');
    expect(useChatStore.getState().messages.map((message) => message.content)).toEqual(['B正文']);
    expect(useChatStore.getState().isHistoryView).toBe(false);
    expect(useToastStore.getState().toasts).toEqual([]);
  });

  it('preserves an internal tool record marker without presenting it as an assistant reply', async () => {
    mocks.getConversation.mockResolvedValue({
      messages: [{ ...canonicalHistoryResult('内部原记录'), internal_agent: true }],
    });
    await useConversationStore.getState().loadConversation('canonical-history');
    const record = useChatStore.getState().messages[0];
    expect(record).toMatchObject({ role: 'tool', internalAgent: true, content: '内部原记录' });
    if (!record) throw new Error('missing tool record');
    render(
      createElement(MessageBubble, { message: record, onFork: vi.fn(), onRegenerate: vi.fn() })
    );
    expect(screen.getByText('Agent 内部工具记录')).toBeDefined();
    expect(screen.queryByRole('button', { name: '分叉会话' })).toBeNull();
  });

  it('keeps orphan records as tool rows without stealing assistant actions or Subagent ownership', async () => {
    mocks.getConversation.mockResolvedValue({
      messages: [
        { message_id: 'assistant-before', role: 'assistant', content: '前一条正文' },
        canonicalHistoryResult('未关联结果正文'),
        {
          message_id: 'assistant-after',
          role: 'assistant',
          content: '后一条真实回复',
          final_answer_confirmed: true,
        },
      ],
    });
    await useConversationStore.getState().loadConversation('canonical-history');
    const messages = useChatStore.getState().messages;
    const tool = messages.find((message) => message.role === 'tool');
    expect(tool?.content).toBe('未关联结果正文');
    useSubagentRunStore.getState().ingest({
      kind: 'subagent',
      workspace_id: 'global',
      conversation_id: 'canonical-history',
      subagent_run_id: 'legacy-subagent',
      run_id: 'legacy-run',
      task_id: 'legacy-task',
      agent: 'reviewer',
      event: 'completed',
      summary: '真实 Subagent 结果',
    });
    render(
      createElement(
        'div',
        {},
        messages.map((message) =>
          createElement(
            'div',
            { key: message.id, 'data-testid': message.id },
            createElement(MessageBubble, { message, onFork: vi.fn(), onRegenerate: vi.fn() })
          )
        )
      )
    );
    if (!tool) throw new Error('missing original tool record');
    const recordRow = within(screen.getByTestId(tool.id));
    expect(recordRow.queryByRole('button', { name: '分叉会话' })).toBeNull();
    expect(recordRow.queryByRole('button', { name: '重新生成' })).toBeNull();
    expect(recordRow.queryByRole('button', { name: /Subagent/ })).toBeNull();
    expect(recordRow.getByRole('button', { name: '复制原记录' })).toBeDefined();
    const finalRow = within(screen.getByTestId('assistant-after'));
    expect(finalRow.getByRole('button', { name: '分叉会话' })).toBeDefined();
    expect(finalRow.getByRole('button', { name: '重新生成' })).toBeDefined();
    fireEvent.click(finalRow.getByRole('button', { name: '展开执行过程' }));
    expect(finalRow.getByRole('button', { name: /Subagent.*reviewer/ })).toBeDefined();
    expect(
      within(screen.getByTestId('assistant-before')).queryByRole('button', { name: /Subagent/ })
    ).toBeNull();
  });

  it('retains the selected conversation when malformed history fails and reports a retryable error', async () => {
    useConversationStore.setState({ activeId: 'old-selection' });
    const messages: ChatMessage[] = [
      { id: 'old-message', role: 'assistant', content: '原对话内容', timestamp: 1 },
    ];
    useChatStore.setState({ messages });
    mocks.getConversation.mockRejectedValueOnce(new Error('tool history parse failed'));
    expect(await useConversationStore.getState().loadConversation('bad-history')).toBe(false);
    expect(useConversationStore.getState().activeId).toBe('old-selection');
    expect(useChatStore.getState().messages).toBe(messages);
    expect(useConversationStore.getState().isLoading).toBe(false);
    expect(useToastStore.getState().toasts).toEqual([
      expect.objectContaining({
        type: 'error',
        message: '无法加载会话或工具历史；原始记录保留，请重试',
      }),
    ]);
  });

  it.each(['before-restore', 'after-restore'])(
    'returns false and preserves the old view when projection is refused %s',
    async (phase) => {
      useConversationStore.setState({ activeId: 'retained-view' });
      const messages: ChatMessage[] = [
        { id: 'retained-message', role: 'assistant', content: '保留旧视图', timestamp: 1 },
      ];
      useChatStore.setState({ messages });
      mocks.getConversation.mockResolvedValueOnce({ messages: [] });
      let canProject = phase !== 'before-restore';
      if (phase === 'after-restore')
        mocks.restoreConversation.mockImplementationOnce(async () => {
          canProject = false;
        });
      expect(
        await useConversationStore.getState().loadConversation('refused-view', () => canProject)
      ).toBe(false);
      expect(useConversationStore.getState()).toMatchObject({
        activeId: 'retained-view',
        isLoading: false,
      });
      expect(useChatStore.getState().messages).toBe(messages);
      expect(useToastStore.getState().toasts).toEqual([]);
      if (phase === 'before-restore') expect(mocks.restoreConversation).not.toHaveBeenCalled();
      else
        expect(mocks.restoreConversation).toHaveBeenCalledExactlyOnceWith('global', 'refused-view');
    }
  );

  it('does not send a failed old request toast into a newer selected conversation', async () => {
    let rejectOld: (error: Error) => void = () => undefined;
    const old = new Promise<never>((_resolve, reject) => {
      rejectOld = reject;
    });
    mocks.getConversation
      .mockReturnValueOnce(old)
      .mockResolvedValueOnce({ messages: [{ role: 'assistant', content: '新会话正文' }] });
    const pending = useConversationStore.getState().loadConversation('bad-history');
    await useConversationStore.getState().loadConversation('new-selection');
    rejectOld(new Error('late old parse error'));
    await pending;
    expect(useConversationStore.getState().activeId).toBe('new-selection');
    expect(useChatStore.getState().messages.map((message) => message.content)).toEqual([
      '新会话正文',
    ]);
    expect(useToastStore.getState().toasts).toEqual([]);
  });

  it.each([
    { workspace_id: 'foreign-workspace' },
    { conversation_id: 'foreign-conversation' },
    { owner: { kind: 'chat', message_id: 'foreign-owner' } },
    { owner: { kind: 'subagent', subagent_run_id: 'foreign-subagent' } },
    { name: 'other-tool' },
  ])('does not borrow a status across an identity boundary: %j', async (foreign) => {
    mocks.getConversation.mockResolvedValue({ messages: [canonicalHistoryCall()] });
    mocks.listToolExecutions.mockResolvedValue([canonicalHistorySummary(foreign)]);
    await useConversationStore.getState().loadConversation('canonical-history');
    const message = useChatStore.getState().messages[0];
    expect(
      message?.executionSteps?.some(
        (step) => step.type === 'tool' && step.callId === 'known-detail'
      )
    ).toBe(false);
    if (!message) throw new Error('missing history');
    render(createElement(MessageBubble, { message }));
    fireEvent.click(screen.getByRole('button', { name: '展开执行过程' }));
    expect(screen.getByText('历史执行状态未恢复')).toBeDefined();
  });

  it('does not infer the original owner from a unique call id in an older message', async () => {
    mocks.getConversation.mockResolvedValue({
      messages: [canonicalHistoryCall({ message_id: undefined })],
    });
    mocks.listToolExecutions.mockResolvedValue([canonicalHistorySummary()]);
    await useConversationStore.getState().loadConversation('canonical-history');
    expect(
      useChatStore
        .getState()
        .messages[0]?.executionSteps?.some(
          (step) => step.type === 'tool' && step.callId === 'known-detail'
        )
    ).toBe(false);
  });

  it('preserves duplicate calls and an unpaired result as separate unknown records', async () => {
    mocks.getConversation.mockResolvedValue({
      messages: [
        canonicalHistoryCall(),
        canonicalHistoryCall({ message_id: 'second-owner' }),
        canonicalHistoryResult('重复身份的原始记录'),
      ],
    });
    mocks.listToolExecutions.mockResolvedValue([canonicalHistorySummary()]);
    await useConversationStore.getState().loadConversation('canonical-history');
    const messages = useChatStore.getState().messages;
    expect(messages).toHaveLength(3);
    expect(
      messages
        .flatMap((message) => message.transcriptTools ?? [])
        .every((record) => record.identityAmbiguous)
    ).toBe(true);
    expect(messages[0]?.transcriptTools?.[0]?.recordedOutput).toBeUndefined();
    expect(messages[1]?.transcriptTools?.[0]?.recordedOutput).toBeUndefined();
    expect(messages[2]?.transcriptTools?.[0]?.recordedOutput).toBe('重复身份的原始记录');
    expect(
      new Set(
        messages.flatMap((message) => message.transcriptTools?.map((record) => record.id) ?? [])
      ).size
    ).toBe(3);
  });

  it('does not fabricate a request or status for an orphan result', async () => {
    mocks.getConversation.mockResolvedValue({ messages: [canonicalHistoryResult('孤立工具正文')] });
    await useConversationStore.getState().loadConversation('canonical-history');
    expect(useChatStore.getState().messages[0]).toMatchObject({
      role: 'tool',
      content: '孤立工具正文',
      transcriptTools: [
        expect.objectContaining({ recordedOutput: '孤立工具正文', identityAmbiguous: true }),
      ],
    });
    expect(
      useChatStore.getState().messages[0]?.transcriptTools?.[0]?.requestedArguments
    ).toBeUndefined();
    expect(Object.values(useToolExecutionStore.getState().tools)).toEqual([]);
  });

  it('does not duplicate existing execution references when canonical calls are present', async () => {
    mocks.getConversation.mockResolvedValue({
      messages: [
        canonicalHistoryCall({ execution_steps: [{ type: 'tool', call_id: 'known-detail' }] }),
        canonicalHistoryResult(),
      ],
    });
    mocks.listToolExecutions.mockResolvedValue([canonicalHistorySummary()]);
    await useConversationStore.getState().loadConversation('canonical-history');
    expect(useChatStore.getState().messages[0]?.executionSteps).toEqual([
      { type: 'tool', callId: 'known-detail' },
    ]);
  });

  it('keeps a real running execution live and adopts only its real later terminal event', async () => {
    const running = canonicalHistorySummary({
      status: 'running',
      finished_at: null,
      duration_ms: null,
    });
    mocks.getConversation.mockResolvedValue({
      messages: [canonicalHistoryCall(), canonicalHistoryResult('[Error] is not proof of failure')],
    });
    mocks.listToolExecutions.mockResolvedValue([running]);
    await useConversationStore.getState().loadConversation('canonical-history');
    expect(useToolExecutionStore.getState().tools['known-detail']).toEqual(running);
    const message = useChatStore.getState().messages[0];
    if (!message) throw new Error('missing history');
    render(createElement(MessageBubble, { message }));
    expect(screen.queryByText('已完成', { exact: true })).toBeNull();
    expect(screen.queryByRole('button', { name: '展开执行过程' })).toBeNull();
    expect(screen.getByText('正在执行 1 个工具')).toBeDefined();
    expect(screen.queryByText(/1 个失败/)).toBeNull();
  });

  it('keeps a new active stream when an old history request returns late', async () => {
    const old = deferred<{
      messages: ReturnType<typeof canonicalHistoryCall>[];
    }>();
    mocks.getConversation.mockReturnValueOnce(old.promise);
    const pending = useConversationStore.getState().loadConversation('canonical-history');
    await useConversationStore.getState().startNew();
    const id = useChatStore.getState().startAssistantMessage('new-stream');
    useChatStore.getState().appendToken(id, '新流式正文');
    old.resolve({ messages: [canonicalHistoryCall()] });
    await pending;
    expect(useChatStore.getState().messages.map((message) => message.content)).toEqual([
      '新流式正文',
    ]);
    expect(useChatStore.getState().isStreaming).toBe(true);
  });

  it('recovers call steps against an exact workspace/conversation/owner summary', async () => {
    mocks.getConversation.mockResolvedValue({
      messages: [canonicalHistoryCall(), canonicalHistoryResult()],
    });
    mocks.listToolExecutions.mockResolvedValue([canonicalHistorySummary()]);
    await useConversationStore.getState().loadConversation('canonical-history');
    const message = useChatStore.getState().messages[0];
    expect(message?.executionSteps).toEqual([{ type: 'tool', callId: 'known-detail' }]);
    expect(useToolExecutionStore.getState().tools['known-detail']?.status).toBe('failed');
  });

  it('keeps unknown call details on the message without adding synthetic status to the live store', async () => {
    mocks.getConversation.mockResolvedValue({
      messages: [
        canonicalHistoryCall(),
        canonicalHistoryResult('{"success":true,"output":"原记录"}'),
      ],
    });
    await useConversationStore.getState().loadConversation('canonical-history');
    const message = useChatStore.getState().messages[0];
    expect(message).toMatchObject({
      transcriptTools: [
        expect.objectContaining({
          callId: 'canonical-call',
          name: 'shell',
          requestedArguments: '{"command":"echo 中文 🧭"}',
          recordedOutput: '{"success":true,"output":"原记录"}',
        }),
      ],
    });
    expect(Object.values(useToolExecutionStore.getState().tools)).toEqual([]);
    if (!message) throw new Error('missing historical message');
    render(createElement(MessageBubble, { message }));
    expect(screen.getByText('历史执行状态未恢复')).toBeDefined();
    fireEvent.click(screen.getByRole('button', { name: '展开执行过程' }));
    fireEvent.click(screen.getByRole('button', { name: '展开工具执行过程' }));
    fireEvent.click(screen.getByRole('button', { name: /shell.*状态未恢复/ }));
    expect(screen.getByText('原始请求参数')).toBeDefined();
    expect(screen.getByText('{"command":"echo 中文 🧭"}')).toBeDefined();
    expect(screen.getByText('{"success":true,"output":"原记录"}')).toBeDefined();
    expect(screen.queryByText('success', { exact: true })).toBeNull();
    expect(screen.queryByText(/\d+\.\d+s/)).toBeNull();
  });
});

it('places a newly persisted Agent input after real cold human/assistant history, not before load-time timestamps', async () => {
  const { agentInputMessages, withAgentInputs } = await import('../lib/agentCommunication');
  useConversationStore.setState({ workspaceId: 'global', activeId: null, newConversationEpoch: 1 });
  mocks.getConversation.mockResolvedValueOnce({
    messages: [
      {
        message_id: 'human',
        role: 'user',
        content: 'earlier human',
        created_at: '2026-10-28T00:00:00Z',
      },
      {
        message_id: 'reply',
        role: 'assistant',
        content: 'earlier assistant',
        created_at: '2026-10-28T00:00:01Z',
      },
    ],
  });
  await useConversationStore.getState().loadConversation('cold-target');
  const messages = useChatStore.getState().messages;
  expect(messages.map((message) => message.timestamp)).toEqual([
    Date.parse('2026-10-28T00:00:00Z'),
    Date.parse('2026-10-28T00:00:01Z'),
  ]);
  const record = {
    message_id: 'pending-agent',
    route: { workspace_id: 'global', conversation_id: 'cold-target' },
    payload: {
      message_id: 'pending-agent',
      from: { workspace_id: 'source', conversation_id: 'sender' },
      to: { workspace_id: 'global', conversation_id: 'cold-target' },
      origin: 'agent' as const,
      payload: { kind: 'text' as const, text: 'persisted after old history' },
      created_at: '2026-10-28T00:00:02Z',
    },
    persisted_at: '2026-10-28T00:00:02Z',
    phase: 'persisted',
    outcome: null,
    drained: false,
    reason: null,
    turn_id: null,
    attempt: 0,
  };
  const timeline = withAgentInputs(messages, agentInputMessages([record], 'global', 'cold-target'));
  expect(timeline.map((message) => message.content)).toEqual([
    'earlier human',
    'earlier assistant',
    'persisted after old history',
  ]);
  expect(useChatStore.getState().messages).toHaveLength(2);
});

it.each(['original', 'fork-copy'] as const)(
  'keeps canonical target chronology and original Agent identity through %s cold/live merging',
  async (mode) => {
    const { agentInputMessages, withAgentInputs } = await import('../lib/agentCommunication');
    const conversationId = mode === 'original' ? 'original-target' : 'fork-target';
    const canonicalAt = '2026-10-28T00:00:30Z';
    const source = {
      message_id: 'accepted-agent',
      from: { workspace_id: 'sender-workspace', conversation_id: 'sender-chat' },
      to:
        mode === 'original'
          ? { workspace_id: 'global', conversation_id: conversationId }
          : { workspace_id: 'source-workspace', conversation_id: 'source-chat' },
      origin: 'agent' as const,
      payload: { kind: 'text' as const, text: '原始 Agent 正文 🧪' },
      created_at: '2026-10-28T00:00:01Z',
    };
    mocks.getConversation.mockResolvedValueOnce({
      messages: [
        {
          message_id: 'human',
          role: 'user',
          content: 'human before acceptance',
          created_at: '2026-10-28T00:00:10Z',
        },
        {
          message_id: 'canonical-agent',
          role: 'user',
          content: 'runtime guidance',
          created_at: canonicalAt,
          agent_input: source,
          attachments: [
            {
              name: 'context.txt',
              mime_type: 'text/plain',
              url: 'data:text/plain;base64,Y29udGV4dA==',
              size: 7,
            },
          ],
        },
        {
          message_id: 'answer',
          role: 'assistant',
          content: 'assistant after acceptance',
          created_at: '2026-10-28T00:00:40Z',
        },
      ],
    });
    await useConversationStore.getState().loadConversation(conversationId);
    const cold = useChatStore.getState().messages;
    const accepted = cold.find((message) => message.agentInput);
    expect(accepted?.timestamp).toBe(Date.parse(canonicalAt));
    expect(accepted?.agentInput?.payload).toEqual(source);
    expect(accepted?.internalAgent).toBe(true);
    expect(accepted?.attachments?.[0]?.name).toBe('context.txt');
    const pending = {
      message_id: 'pending-agent',
      route: { workspace_id: 'global', conversation_id: conversationId },
      payload: {
        ...source,
        message_id: 'pending-agent',
        to: { workspace_id: 'global', conversation_id: conversationId },
        payload: { kind: 'text' as const, text: 'pending between canonical rows' },
      },
      persisted_at: '2026-10-28T00:00:20Z',
      phase: 'persisted',
      outcome: null,
      drained: false,
      reason: null,
      turn_id: null,
      attempt: 0,
    };
    const timeline = withAgentInputs(cold, agentInputMessages([pending], 'global', conversationId));
    expect(timeline.map((message) => message.content)).toEqual([
      'human before acceptance',
      'pending between canonical rows',
      source.payload.text,
      'assistant after acceptance',
    ]);
    expect(useChatStore.getState().messages).toBe(cold);
    if (!accepted) throw new Error('canonical Agent input fixture was not restored');
    render(
      createElement(MessageBubble, { message: accepted, onEditAndResend: vi.fn(), onFork: vi.fn() })
    );
    expect(screen.getByText('来自 Agent · sender-workspace/sender-chat')).toBeDefined();
    expect(screen.queryByRole('button', { name: '编辑' })).toBeNull();
    expect(screen.queryByRole('button', { name: '分叉会话' })).toBeNull();
    expect(mocks.createSideConversation).not.toHaveBeenCalled();
  }
);

it('does not create or rename a conversation from an Agent observation alone', async () => {
  const { restoredAgentInput } = await import('../lib/agentCommunication');
  const payload = {
    message_id: 'agent-only',
    from: { workspace_id: 'source', conversation_id: 'sender' },
    to: { workspace_id: 'global', conversation_id: 'existing' },
    origin: 'agent' as const,
    payload: { kind: 'text' as const, text: 'not a human title' },
    created_at: '2026-10-28T00:00:00Z',
  };
  useConversationStore.setState({ activeId: 'existing', workspaceId: 'global' });
  const record: ChatMessage = {
    role: 'user',
    ...restoredAgentInput(payload, 'global', 'existing'),
    timestamp: Date.parse('2026-10-28T00:00:10Z'),
  };
  expect(await useConversationStore.getState().saveCurrent([record])).toEqual({
    workspace_id: 'global',
    conversation_id: 'existing',
  });
  expect(mocks.updateConversation).not.toHaveBeenCalled();
  expect(mocks.saveConversation).not.toHaveBeenCalled();
});
