// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { TaskRun } from '../../generated';
import type { Workspace } from '../../api/endpoints';

const mocks = vi.hoisted(() => {
  Object.defineProperty(window, 'matchMedia', {
    configurable: true,
    value: () => ({ matches: false }),
  });
  return { send: vi.fn(), models: vi.fn(), latest: vi.fn() };
});
vi.mock('../../hooks/useTauriChat', () => ({
  useTauriChat: () => ({
    queuedInputs: [],
    sendMessage: mocks.send,
    cancel: vi.fn(),
    sendApproval: vi.fn(),
    sendInput: vi.fn(),
    sendSelection: vi.fn(),
  }),
}));
vi.mock('./WelcomeScreen', () => ({ WelcomeScreen: () => null }));
vi.mock('./HitlDialog', () => ({ HitlDialog: () => null }));
vi.mock('../../api/endpoints', async (original) => {
  const actual = await original<typeof import('../../api/endpoints')>();
  return {
    ...actual,
    providerApi: {
      ...actual.providerApi,
      listConfigured: mocks.models,
      listProviders: vi.fn().mockResolvedValue({ providers: [] }),
      setThinking: vi.fn().mockResolvedValue({ success: true }),
    },
    permissionsApi: {
      ...actual.permissionsApi,
      getMode: vi.fn().mockResolvedValue({ mode: 'default' }),
    },
    conversationApi: { ...actual.conversationApi, agentInputs: vi.fn().mockResolvedValue([]) },
    taskRuntimeApi: { ...actual.taskRuntimeApi, latestRunForConversation: mocks.latest },
  };
});

import { ChatPanel } from './ChatPanel';
import { useWorkspaceStore } from '../../stores/workspaceStore';
import { useConversationStore } from '../../stores/conversationStore';
import { useTaskRuntimeStore } from '../../stores/taskRuntimeStore';
import { useChatStore } from '../../stores/chatStore';
import { useUiStore } from '../../stores/uiStore';
import { useToastStore } from '../../stores/toastStore';

const workspace: Workspace = {
  id: 'slash-workspace',
  name: 'Slash',
  root: '/data/slash',
  project_root: '/code/slash',
  product_data_generation: 'slash-v1',
  kind: { type: 'code' },
  metadata: { tags: [], project_location: { kind: 'local', path: '/code/slash' } },
  created_at: '',
  last_active: '',
};
const run: TaskRun = {
  run_id: 'slash-run',
  workspace_id: workspace.id,
  conversation_id: 'slash-parent',
  root_message_id: 'root',
  goal: '原目标 🧭',
  goal_revision: 2,
  goal_sha256: 'hash',
  status: 'paused',
  plan_id: null,
  route: 'diagnostic',
  attended_mode: 'attended',
  domain_profile: 'general',
  domain_binding: null,
  created_at: '',
  updated_at: '',
};
const taskActions = useTaskRuntimeStore.getState();
const conversationActions = useConversationStore.getState();
const update = vi.fn();
const pause = vi.fn();
const createSide = vi.fn();
const refresh = vi.fn();

beforeEach(() => {
  vi.clearAllMocks();
  localStorage.clear();
  Object.defineProperty(window, 'matchMedia', {
    configurable: true,
    value: () => ({ matches: false }),
  });
  Object.defineProperty(HTMLElement.prototype, 'scrollIntoView', {
    configurable: true,
    value: vi.fn(),
  });
  vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) => {
    callback(0);
    return 1;
  });
  useWorkspaceStore.setState({ current: workspace, isLoading: false });
  useConversationStore.setState({
    workspaceId: workspace.id,
    activeId: run.conversation_id,
    newConversationEpoch: 1,
    isLoading: false,
    conversations: [],
    sideConversations: [],
    createSideConversation: createSide,
  });
  useUiStore.setState({ navigationEpoch: 0 });
  useChatStore.setState({
    messages: [],
    pendingHitlRequests: [],
    isStreaming: false,
    isCancelled: false,
    activity: null,
    runStatus: 'idle',
  });
  useTaskRuntimeStore.setState({
    activeRun: { ...run },
    taskScope: {
      workspaceId: workspace.id,
      workspaceGeneration: workspace.product_data_generation,
    },
    todos: [],
    continuation: null,
    backgroundCells: [],
    error: null,
    updateGoal: update,
    pause,
    refresh,
  });
  useToastStore.setState({ toasts: [] });
  refresh.mockResolvedValue(undefined);
  mocks.latest.mockImplementation(async () => useTaskRuntimeStore.getState().activeRun);
  update.mockImplementation(async (_target, revision: number, goal: string) => {
    useTaskRuntimeStore.setState({ activeRun: { ...run, goal, goal_revision: revision + 1 } });
    return true;
  });
  pause.mockImplementation(async () => {
    useTaskRuntimeStore.setState({ activeRun: { ...run } });
    return { success: true, run_id: run.run_id };
  });
  createSide.mockResolvedValue({ conversation_id: 'child' });
  mocks.models.mockResolvedValue({
    default_model_id: 'provider:model',
    models: [
      {
        id: 'provider:model',
        display_name: 'Model',
        provider: 'provider',
        model: 'model',
        enabled: true,
        is_default: true,
        thinking_levels: [],
        input_modalities: ['text'],
      },
    ],
  });
});
afterEach(() => {
  cleanup();
  useTaskRuntimeStore.setState({
    updateGoal: taskActions.updateGoal,
    pause: taskActions.pause,
    refresh: taskActions.refresh,
  });
  useConversationStore.setState({
    createSideConversation: conversationActions.createSideConversation,
  });
  vi.unstubAllGlobals();
});

async function editor() {
  render(<ChatPanel />);
  await waitFor(() => expect(mocks.models).toHaveBeenCalled());
  return screen.getByRole('textbox', { name: '消息' }) as HTMLTextAreaElement;
}
function send(input: HTMLTextAreaElement, text: string) {
  // Space also exercises the real palette -> command submission boundary.
  fireEvent.change(input, { target: { value: text } });
  fireEvent.keyDown(input, { key: 'Enter' });
}

describe('Eclip slash commands through the real composer', () => {
  it('opens the Goal editor without a model or a model turn', async () => {
    mocks.models.mockResolvedValue({ default_model_id: null, models: [] });
    const input = await editor();
    send(input, '/goal ');
    const goal = await screen.findByRole('textbox', { name: '编辑当前目标' });
    expect((goal as HTMLInputElement).value).toBe(run.goal);
    expect(update).not.toHaveBeenCalled();
    expect(mocks.send).not.toHaveBeenCalled();
    await waitFor(() => expect(input.value).toBe(''));
  });

  it('submits a Unicode Goal directly through the original pause, CAS and canonical refresh', async () => {
    useTaskRuntimeStore.setState({ activeRun: { ...run, status: 'running' } });
    const input = await editor();
    send(input, '/goal 新目标 中文 🚀');
    await screen.findByText(/目标已更新；/);
    expect(pause).toHaveBeenCalledOnce();
    expect(update).toHaveBeenCalledWith(
      {
        workspace_id: workspace.id,
        workspace_generation: workspace.product_data_generation,
        run_id: run.run_id,
      },
      2,
      '新目标 中文 🚀',
      '用户更新目标'
    );
    expect(refresh).toHaveBeenCalled();
    expect(mocks.send).not.toHaveBeenCalled();
    await waitFor(() => expect(input.value).toBe(''));
  });

  it('retains both command and edit draft on update failure', async () => {
    update.mockRejectedValue(new Error('版本冲突'));
    const input = await editor();
    send(input, '/goal 保留失败草稿');
    await screen.findByRole('alert');
    await waitFor(() =>
      expect(
        useToastStore.getState().toasts.some((item) => item.message.includes('目标尚未更新'))
      ).toBe(true)
    );
    expect(input.value).toBe('/goal 保留失败草稿');
    expect((screen.getByRole('textbox', { name: '编辑当前目标' }) as HTMLInputElement).value).toBe(
      '保留失败草稿'
    );
    expect(update).toHaveBeenCalledOnce();
    expect(mocks.send).not.toHaveBeenCalled();
  });

  it('opens Side without a model and confirms creation only when a problem is entered', async () => {
    mocks.models.mockResolvedValue({ default_model_id: null, models: [] });
    const input = await editor();
    send(input, '/side ');
    await screen.findByRole('dialog', { name: '新建支线对话' });
    expect(createSide).not.toHaveBeenCalled();
    expect(mocks.send).not.toHaveBeenCalled();
  });

  it('creates and starts Side directly, preserves multiline case and reuses retry identity', async () => {
    createSide.mockRejectedValueOnce(new Error('网络中断'));
    const input = await editor();
    send(input, '/SIDE 调研 Responses\n第二行 🧭');
    await waitFor(() => expect(createSide).toHaveBeenCalledOnce());
    await waitFor(() =>
      expect(
        useToastStore.getState().toasts.some((item) => item.message.includes('网络中断'))
      ).toBe(true)
    );
    expect(input.value).toBe('/SIDE 调研 Responses\n第二行 🧭');
    fireEvent.keyDown(input, { key: 'Enter' });
    await waitFor(() => expect(createSide).toHaveBeenCalledTimes(2));
    const first = createSide.mock.calls[0]?.[0];
    const retry = createSide.mock.calls[1]?.[0];
    expect(first.prompt).toBe('调研 Responses\n第二行 🧭');
    expect(retry.requestId).toBe(first.requestId);
    expect(first.source.conversationId).toBe(run.conversation_id);
    expect(mocks.send).not.toHaveBeenCalled();
  });

  it('starts a new Goal using the original send path with the slash prefix removed', async () => {
    useTaskRuntimeStore.setState({ activeRun: null });
    mocks.send.mockResolvedValue(true);
    const input = await editor();
    send(input, '/goal 实现新目标');
    await waitFor(() => expect(mocks.send).toHaveBeenCalledOnce());
    expect(mocks.send.mock.calls[0]?.[0]).toBe('实现新目标');
    expect(mocks.send.mock.calls[0]?.[3].payload.text).toBe('实现新目标');
  });

  it('opens a new Goal form and sends only after confirmation', async () => {
    useTaskRuntimeStore.setState({ activeRun: null });
    mocks.send.mockImplementation(async (_text, _attachments, _domain, submission) => {
      submission.onDirectAccepted(() => true);
      return true;
    });
    const input = await editor();
    send(input, '/goal ');
    await screen.findByRole('dialog', { name: '创建目标' });
    expect(mocks.send).not.toHaveBeenCalled();
    fireEvent.change(screen.getByRole('textbox', { name: '目标内容' }), {
      target: { value: '用户明确创建的新目标' },
    });
    fireEvent.click(screen.getByRole('button', { name: '创建并开始' }));
    await waitFor(() => expect(mocks.send).toHaveBeenCalledOnce());
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    expect(mocks.send.mock.calls[0]?.[0]).toBe('用户明确创建的新目标');
  });

  it('clears the accepted Side command before opening its child', async () => {
    createSide.mockImplementationOnce(async ({ onCreated }) => {
      onCreated(() => true);
      useConversationStore.setState({ activeId: 'child' });
      return { conversation_id: 'child' };
    });
    const input = await editor();
    send(input, '/side 分析支线');
    await waitFor(() => expect(useConversationStore.getState().activeId).toBe('child'));
    expect(input.value).toBe('');
    expect(mocks.send).not.toHaveBeenCalled();
  });

  it('opens Side and uses its original creation action after confirmation', async () => {
    const input = await editor();
    send(input, '/side ');
    await screen.findByRole('dialog', { name: '新建支线对话' });
    await screen.findByRole('option', { name: /Model$/ });
    fireEvent.change(screen.getByRole('textbox', { name: '要处理的问题' }), {
      target: { value: '确认后开始的支线' },
    });
    fireEvent.click(screen.getByRole('button', { name: '创建' }));
    await waitFor(() => expect(createSide).toHaveBeenCalledOnce());
    expect(createSide.mock.calls[0]?.[0]).toMatchObject({
      prompt: '确认后开始的支线',
      source: { conversationId: run.conversation_id },
    });
    expect(mocks.send).not.toHaveBeenCalled();
  });

  it('closes a fresh Goal form when the user selects another conversation', async () => {
    useTaskRuntimeStore.setState({ activeRun: null });
    useConversationStore.setState({ activeId: null });
    const input = await editor();
    send(input, '/goal ');
    await screen.findByRole('dialog', { name: '创建目标' });
    act(() => useConversationStore.setState({ activeId: 'another-conversation' }));
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    expect(mocks.send).not.toHaveBeenCalled();
  });

  it('loads an uncached active Goal and retains the command instead of steering ordinary chat', async () => {
    useTaskRuntimeStore.setState({ activeRun: null });
    mocks.latest.mockResolvedValue({ ...run });
    const load = vi
      .spyOn(useTaskRuntimeStore.getState(), 'loadByConversation')
      .mockImplementation(async () => {
        useTaskRuntimeStore.setState({ activeRun: { ...run } });
      });
    try {
      const input = await editor();
      send(input, '/goal 缓存缺失时仍修改目标');
      await waitFor(() => expect(load).toHaveBeenCalledWith(workspace.id, run.conversation_id));
      await waitFor(() =>
        expect(
          useToastStore.getState().toasts.some((item) => item.message.includes('重新加载'))
        ).toBe(true)
      );
      expect(mocks.send).not.toHaveBeenCalled();
      expect(screen.queryByRole('dialog', { name: '创建目标' })).toBeNull();
      expect(input.value).toBe('/goal 缓存缺失时仍修改目标');
      fireEvent.keyDown(input, { key: 'Enter' });
      await screen.findByText(/目标已更新；/);
      expect(update).toHaveBeenCalledOnce();
    } finally {
      load.mockRestore();
    }
  });

  it('does not treat a failed Goal read as permission to create or steer', async () => {
    useTaskRuntimeStore.setState({ activeRun: null });
    mocks.latest.mockRejectedValue(new Error('目标读取失败'));
    const input = await editor();
    send(input, '/goal 禁止误发');
    await waitFor(() =>
      expect(
        useToastStore.getState().toasts.some((item) => item.message.includes('目标读取失败'))
      ).toBe(true)
    );
    expect(input.value).toBe('/goal 禁止误发');
    expect(mocks.send).not.toHaveBeenCalled();
    expect(update).not.toHaveBeenCalled();
    expect(screen.queryByRole('dialog', { name: '创建目标' })).toBeNull();
  });

  it('does not create or steer when a Goal appears during the model check', async () => {
    useTaskRuntimeStore.setState({ activeRun: null });
    const input = await editor();
    let release: (value: unknown) => void = () => undefined;
    mocks.models.mockReturnValueOnce(
      new Promise((resolve) => {
        release = resolve;
      })
    );
    const calls = mocks.models.mock.calls.length;
    send(input, '/goal 新运行出现时保留');
    await waitFor(() => expect(mocks.models.mock.calls.length).toBeGreaterThan(calls));
    act(() => useTaskRuntimeStore.setState({ activeRun: { ...run } }));
    await act(async () => release({ models: [{ enabled: true }] }));
    await waitFor(() =>
      expect(
        useToastStore.getState().toasts.some((item) => item.message.includes('活动目标'))
      ).toBe(true)
    );
    expect(mocks.send).not.toHaveBeenCalled();
    expect(input.value).toBe('/goal 新运行出现时保留');
  });

  it.each([false, true])(
    'binds only its own first conversation and preserves the Goal form on failure (leave=%s)',
    async (leave) => {
      useTaskRuntimeStore.setState({ activeRun: null });
      useConversationStore.setState({ activeId: null });
      let release: (value: boolean) => void = () => undefined;
      mocks.send.mockImplementationOnce(async (_text, _attachments, _domain, submission) => {
        submission.onConversationCreated({
          workspace_id: workspace.id,
          conversation_id: 'own-created',
        });
        useConversationStore.setState({ activeId: 'own-created' });
        return new Promise<boolean>((resolve) => {
          release = resolve;
        });
      });
      const input = await editor();
      send(input, '/goal ');
      const draft = (await screen.findByRole('textbox', {
        name: '目标内容',
      })) as HTMLTextAreaElement;
      fireEvent.change(draft, { target: { value: '创建后发送失败仍保留' } });
      fireEvent.click(screen.getByRole('button', { name: '创建并开始' }));
      await waitFor(() => expect(useConversationStore.getState().activeId).toBe('own-created'));
      expect(screen.getByRole('dialog', { name: '创建目标' })).toBeTruthy();
      if (leave) {
        act(() => useConversationStore.setState({ activeId: 'unrelated' }));
        await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
      }
      await act(async () => release(false));
      if (leave) {
        expect(screen.queryByRole('dialog')).toBeNull();
        expect(useConversationStore.getState().activeId).toBe('unrelated');
      } else {
        await screen.findByRole('alert');
        expect(screen.getByRole('dialog', { name: '创建目标' })).toBeTruthy();
        expect(draft.value).toBe('创建后发送失败仍保留');
      }
    }
  );

  it('keeps a newly opened Side draft when the old dialog receives a late creation result', async () => {
    let release: (value: unknown) => void = () => undefined;
    createSide.mockReturnValueOnce(
      new Promise((resolve) => {
        release = resolve;
      })
    );
    const input = await editor();
    send(input, '/side ');
    await screen.findByRole('option', { name: /Model$/ });
    fireEvent.change(screen.getByRole('textbox', { name: '要处理的问题' }), {
      target: { value: '旧支线' },
    });
    fireEvent.click(screen.getByRole('button', { name: '创建' }));
    await waitFor(() => expect(createSide).toHaveBeenCalledOnce());
    act(() => useConversationStore.setState({ activeId: 'another-parent' }));
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    send(input, '/side ');
    const draft = (await screen.findByRole('textbox', {
      name: '要处理的问题',
    })) as HTMLTextAreaElement;
    fireEvent.change(draft, { target: { value: '保留新支线草稿' } });
    await act(async () => release({ conversation_id: 'old-child' }));
    expect(screen.getByRole('dialog', { name: '新建支线对话' })).toBeTruthy();
    expect(draft.value).toBe('保留新支线草稿');
  });

  it('rejects Side in a child conversation and never sends the command as chat', async () => {
    useConversationStore.setState({
      sideConversations: [
        {
          group_id: 'side-group',
          workspace_id: workspace.id,
          conversation_id: run.conversation_id,
          parent_conversation_id: 'original-parent',
          title: 'Side',
          model_id: null,
          snapshot_message_count: 0,
          snapshot_last_message_id: null,
          created_at: '',
          updated_at: '',
          status: 'idle',
          degraded: false,
          unread_count: 0,
          launch_error: null,
        },
      ],
    });
    const input = await editor();
    send(input, '/side 不能嵌套');
    await waitFor(() =>
      expect(useToastStore.getState().toasts.some((item) => item.message.includes('嵌套'))).toBe(
        true
      )
    );
    expect(createSide).not.toHaveBeenCalled();
    expect(mocks.send).not.toHaveBeenCalled();
    expect(input.value).toBe('/side 不能嵌套');
  });

  it('rejects a late model check after a conversation switch', async () => {
    const input = await editor();
    let release: (value: unknown) => void = () => undefined;
    mocks.models.mockReturnValue(
      new Promise((resolve) => {
        release = resolve;
      })
    );
    const calls = mocks.models.mock.calls.length;
    send(input, '/side 等待中的支线');
    await waitFor(() => expect(mocks.models.mock.calls.length).toBeGreaterThan(calls));
    act(() => useConversationStore.setState({ activeId: 'another-conversation' }));
    await act(async () => release({ models: [{ enabled: true }] }));
    await waitFor(() =>
      expect(screen.getByRole('button', { name: '发送' }).hasAttribute('disabled')).toBe(false)
    );
    expect(createSide).not.toHaveBeenCalled();
    expect(mocks.send).not.toHaveBeenCalled();
    expect(useToastStore.getState().toasts).toEqual([]);
    expect(input.value).toBe('/side 等待中的支线');
  });

  it('does not execute a command while confirming Chinese IME text', async () => {
    const input = await editor();
    fireEvent.change(input, { target: { value: '/goal 新目标' } });
    fireEvent.keyDown(input, { key: 'Enter', isComposing: true });
    fireEvent.keyDown(input, { key: 'Enter', keyCode: 229 });
    expect(update).not.toHaveBeenCalled();
    expect(mocks.send).not.toHaveBeenCalled();
  });
});
