// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { Workspace } from './api/endpoints';

const mocks = vi.hoisted(() => ({
  update: vi.fn(),
  get: vi.fn(),
  exit: vi.fn(),
  create: vi.fn(),
  list: vi.fn(),
  reset: vi.fn(),
  send: vi.fn(),
}));
vi.mock('./api/endpoints', async (original) => {
  const actual = await original<typeof import('./api/endpoints')>();
  const workspace = {
    id: 'a',
    name: 'Workspace A',
    root: '/data/a',
    project_root: '/code/a',
    kind: { type: 'code' },
    metadata: { tags: [], project_location: { kind: 'local' as const, path: '/code/a' } },
    product_data_generation: 'a-v1',
    created_at: '',
    last_active: '',
  };
  return {
    ...actual,
    workspaceApi: {
      ...actual.workspaceApi,
      list: vi.fn().mockResolvedValue({ workspaces: [workspace] }),
      current: vi.fn().mockResolvedValue({ workspace }),
      exit: mocks.exit,
      createAndSwitch: mocks.create,
    },
    conversationApi: {
      ...actual.conversationApi,
      list: mocks.list,
      update: mocks.update,
      get: mocks.get,
      restore: vi.fn().mockResolvedValue({ success: true }),
    },
    toolExecutionApi: { ...actual.toolExecutionApi, list: vi.fn().mockResolvedValue([]) },
    taskRuntimeApi: {
      ...actual.taskRuntimeApi,
      latestRunForConversation: vi.fn().mockResolvedValue(null),
    },
    sessionApi: {
      ...actual.sessionApi,
      getLatest: vi.fn().mockResolvedValue({ found: false }),
      reset: mocks.reset,
    },
    pluginApi: {
      ...actual.pluginApi,
      themes: vi.fn().mockResolvedValue({ themes: [], active: null }),
    },
    providerApi: {
      ...actual.providerApi,
      listProviders: vi.fn().mockResolvedValue({ providers: [] }),
      listConfigured: vi.fn().mockResolvedValue({
        models: [
          {
            id: 'provider:model',
            display_name: 'Model',
            provider: 'provider',
            model: 'model',
            api_protocol: 'responses',
            input_modalities: ['text'],
            enabled: true,
            is_default: true,
            has_auth_token: true,
            auth_source: 'config',
            base_url: 'https://example.test/v1',
            max_tokens: null,
            context_window: 32_000,
            thinking_levels: [],
          },
        ],
      }),
      setThinking: vi.fn().mockResolvedValue({ success: true, spec: 'auto', applied: true }),
    },
    permissionsApi: {
      ...actual.permissionsApi,
      getMode: vi.fn().mockResolvedValue({ mode: 'default' }),
    },
  };
});
vi.mock('./hooks/useTauriChat', () => ({
  useTauriChat: () => ({
    sendMessage: mocks.send,
    sendApproval: vi.fn(),
    sendInput: vi.fn(),
    sendSelection: vi.fn(),
    cancel: vi.fn(),
    queuedInputs: [],
    clearQueuedMessages: vi.fn(),
    removeQueuedMessage: vi.fn(),
    reorderQueuedMessage: vi.fn(),
    steerQueuedMessage: vi.fn(),
  }),
}));
vi.mock('./components/layout/AppLayout', () => ({
  AppLayout: ({ left, center }: { left: React.ReactNode; center: React.ReactNode }) => (
    <div>
      {left}
      {center}
    </div>
  ),
}));
vi.mock('./components/layout/ContextPane', () => ({ ContextPane: () => null }));
vi.mock('./components/layout/SettingsDialog', () => ({ SettingsDialog: () => null }));
vi.mock('./components/task/TaskRuntimePanel', () => ({ InterruptPromptDialog: () => null }));
vi.mock('./components/Auth/RequireAuth', () => ({
  RequireAuth: ({ children }: { children: React.ReactNode }) => children,
}));
vi.mock('./components/chat/HitlDialog', () => ({ HitlDialog: () => null }));
vi.mock('./components/chat/SideConversationDialog', () => ({ SideConversationDialog: () => null }));
vi.mock('./components/analysis/AnalysisPanel', () => ({
  default: () => <div>分析工作台内容</div>,
}));
vi.mock('./components/papers/PaperPanel', () => ({ PaperPanel: () => null }));
vi.mock('./components/workflow/WorkflowPanel', () => ({ WorkflowPanel: () => null }));
vi.mock('./components/extract/ExtractPanel', () => ({ ExtractPanel: () => null }));
vi.mock('./components/common/CommandPalette', () => ({
  default: ({
    isOpen,
    commands,
  }: {
    isOpen: boolean;
    commands: Array<{ id: string; action: () => void }>;
  }) =>
    isOpen ? (
      <button onClick={() => commands.find((item) => item.id === 'open-analysis')?.action()}>
        命令：分析
      </button>
    ) : null,
}));

Object.defineProperty(HTMLElement.prototype, 'scrollIntoView', {
  configurable: true,
  value: vi.fn(),
});
Object.defineProperty(window, 'matchMedia', {
  configurable: true,
  value: () => ({ matches: false }),
});
const { default: App } = await import('./App');
const { useWorkspaceStore } = await import('./stores/workspaceStore');
const { useConversationStore } = await import('./stores/conversationStore');
const { useChatStore } = await import('./stores/chatStore');
const { useUiStore } = await import('./stores/uiStore');
const { useWorkspaceViewStore } = await import('./stores/workspaceViewStore');
const originalClearCurrent = useConversationStore.getState().clearCurrent;

function deferred<T>() {
  let resolve: (value: T) => void = () => undefined;
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}
const workspace: Workspace = {
  id: 'a',
  name: 'Workspace A',
  root: '/data/a',
  project_root: '/code/a',
  kind: { type: 'code' },
  metadata: { tags: [], project_location: { kind: 'local' as const, path: '/code/a' } },
  product_data_generation: 'a-v1',
  created_at: '',
  last_active: '',
};
const row = (scope: string) => ({
  id: 1,
  conversation_id: `${scope}-task`,
  title: scope === 'global' ? '全局任务' : '任务 A',
  message_count: 1,
  created_at: '2026-10-25',
  updated_at: '2026-10-25',
});
afterEach(() => {
  cleanup();
  useConversationStore.setState({ clearCurrent: originalClearCurrent });
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});
beforeEach(() => {
  vi.clearAllMocks();
  vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) => {
    callback(0);
    return 1;
  });
  mocks.list.mockImplementation(async (id: string) => [row(id)]);
  mocks.update.mockResolvedValue({ success: true });
  mocks.get.mockResolvedValue({ messages: [{ role: 'user', content: '已保存的任务内容' }] });
  mocks.exit.mockResolvedValue({ success: true, transition: { status: 'committed' } });
  mocks.reset.mockResolvedValue({ success: true });
  mocks.send.mockResolvedValue(false);
  useWorkspaceStore.setState({ current: workspace, workspaces: [workspace], isLoading: false });
  useConversationStore.setState({
    workspaceId: 'a',
    activeId: null,
    newConversationEpoch: 0,
    conversations: [],
    sideConversations: [],
    isLoading: false,
  });
  useChatStore.getState().clearMessages();
  useUiStore.setState({ navigationEpoch: 0, workspaceDialogOpen: false });
  useWorkspaceViewStore.setState({ activeView: 'chat' });
});

async function ready(view: ReturnType<typeof render>) {
  await waitFor(() =>
    expect((view.getByRole('button', { name: '新建任务' }) as HTMLButtonElement).disabled).toBe(
      false
    )
  );
}

describe('real task and workbench navigation callbacks', () => {
  it('preserves the later App command workbench after new-task metadata save finishes', async () => {
    useConversationStore.setState({ activeId: 'a-task' });
    useChatStore.setState({
      messages: [{ id: 'user', role: 'user', content: '需要保存的原任务', timestamp: 1 }],
    });
    const saved = deferred<{ success: boolean }>();
    mocks.update.mockReturnValueOnce(saved.promise);
    const view = render(<App />);
    await ready(view);
    fireEvent.click(view.getByRole('button', { name: '新建任务' }));
    await waitFor(() => expect(mocks.update).toHaveBeenCalled());
    fireEvent.keyDown(window, { key: 'k', metaKey: true });
    fireEvent.click(view.getByRole('button', { name: '命令：分析' }));
    expect(useWorkspaceViewStore.getState().activeView).toBe('analysis');
    await act(async () => saved.resolve({ success: true }));
    expect(useWorkspaceViewStore.getState().activeView).toBe('analysis');
    expect(mocks.create).not.toHaveBeenCalled();
  });

  it('preserves the later actual ChatPanel menu workbench after a recent-task load finishes', async () => {
    const record = deferred<{ messages: Array<{ role: string; content: string }> }>();
    mocks.get.mockReturnValueOnce(record.promise);
    const view = render(<App />);
    await ready(view);
    fireEvent.click(await view.findByRole('button', { name: '打开任务 任务 A，Workspace A' }));
    await waitFor(() => expect(mocks.get).toHaveBeenCalled());
    fireEvent.click(view.getByRole('button', { name: '打开工作台菜单' }));
    fireEvent.click(view.getByRole('menuitem', { name: '分析' }));
    expect(useWorkspaceViewStore.getState().activeView).toBe('analysis');
    await act(async () =>
      record.resolve({ messages: [{ role: 'user', content: '旧请求的记录' }] })
    );
    expect(useWorkspaceViewStore.getState().activeView).toBe('analysis');
  });

  it('blocks the real new-task button and shortcut during global exit, then starts in global', async () => {
    const exited = deferred<{ success: boolean; transition: { status: string } }>();
    mocks.exit.mockReturnValueOnce(exited.promise);
    const view = render(<App />);
    await ready(view);
    fireEvent.click(await view.findByRole('button', { name: '打开任务 全局任务，未分组任务' }));
    await waitFor(() => expect(useWorkspaceStore.getState().isLoading).toBe(true));
    const epoch = useConversationStore.getState().newConversationEpoch;
    const button = view.getByRole('button', { name: '新建任务' }) as HTMLButtonElement;
    expect(button.disabled).toBe(true);
    fireEvent.click(button);
    fireEvent.keyDown(window, { key: 'o', shiftKey: true, metaKey: true });
    expect(useConversationStore.getState().newConversationEpoch).toBe(epoch);
    await act(async () => exited.resolve({ success: true, transition: { status: 'committed' } }));
    await waitFor(() => expect(useWorkspaceStore.getState().isLoading).toBe(false));
    expect(useWorkspaceStore.getState().current).toBeNull();
    fireEvent.click(view.getByRole('button', { name: '新建任务' }));
    expect(useConversationStore.getState().workspaceId).toBe('global');
    expect(useConversationStore.getState().newConversationEpoch).toBeGreaterThan(epoch);
    expect(mocks.create).not.toHaveBeenCalled();
  });

  it('forwards the original editor descriptor in the Hook fourth slot, retaining Domain as the third slot', async () => {
    useConversationStore.setState({ activeId: 'a-task' });
    const view = render(<App />);
    await ready(view);
    await waitFor(() =>
      expect(view.getByRole('button', { name: '选择模型' }).textContent).toContain('Model')
    );
    const input = view.getByRole('textbox', { name: '消息' }) as HTMLTextAreaElement;
    fireEvent.change(input, { target: { value: '实际普通输入' } });
    fireEvent.keyDown(input, { key: 'Enter' });
    await waitFor(() => expect(mocks.send).toHaveBeenCalledTimes(1));
    const [text, attachments, domainPlugin, submission] = mocks.send.mock.calls[0] ?? [];
    expect(text).toBe('实际普通输入');
    expect(attachments).toBeUndefined();
    expect(domainPlugin).toBeUndefined();
    expect(submission).toMatchObject({
      workspaceId: 'a',
      workspaceGeneration: 'a-v1',
      conversationId: 'a-task',
      newConversationEpoch: 0,
      payload: { text: '实际普通输入' },
    });
    expect(submission.onPersisted).toEqual(expect.any(Function));
    expect(input.value).toBe('实际普通输入');
    expect(view.queryByText('已收到')).toBeNull();
  });

  it('clears the successful local slash draft after its real deferred reset, without a persisted notice', async () => {
    useConversationStore.setState({ activeId: 'a-task' });
    const reset = deferred<{ success: boolean }>();
    mocks.reset.mockReturnValueOnce(reset.promise);
    const view = render(<App />);
    await ready(view);
    await waitFor(() =>
      expect(view.getByRole('button', { name: '选择模型' }).textContent).toContain('Model')
    );
    const input = view.getByRole('textbox', { name: '消息' }) as HTMLTextAreaElement;
    fireEvent.change(input, { target: { value: '/clear ' } });
    fireEvent.keyDown(input, { key: 'Enter' });
    await waitFor(() => expect(mocks.reset).toHaveBeenCalledWith('a', 'a-task'));
    expect(input.value).toBe('/clear ');
    expect(mocks.send).not.toHaveBeenCalled();
    await act(async () => reset.resolve({ success: true }));
    await waitFor(() => expect(input.value).toBe(''));
    expect(useConversationStore.getState().activeId).toBeNull();
    expect(view.queryByText('已收到')).toBeNull();
  });

  it('keeps a newer edit when a successful local slash handler returns late', async () => {
    useConversationStore.setState({ activeId: 'a-task' });
    const reset = deferred<{ success: boolean }>();
    mocks.reset.mockReturnValueOnce(reset.promise);
    const view = render(<App />);
    await ready(view);
    await waitFor(() =>
      expect(view.getByRole('button', { name: '选择模型' }).textContent).toContain('Model')
    );
    const input = view.getByRole('textbox', { name: '消息' }) as HTMLTextAreaElement;
    fireEvent.change(input, { target: { value: '/clear ' } });
    fireEvent.keyDown(input, { key: 'Enter' });
    await waitFor(() => expect(mocks.reset).toHaveBeenCalled());
    fireEvent.change(input, { target: { value: '清理期间继续写的新稿' } });
    await act(async () => reset.resolve({ success: true }));
    expect(input.value).toBe('清理期间继续写的新稿');
    expect(view.queryByText('已收到')).toBeNull();
  });

  it('retains an unsuccessful local-command draft instead of falling through to a model send', async () => {
    const clear = vi
      .spyOn(useConversationStore.getState(), 'clearCurrent')
      .mockRejectedValueOnce(new Error('local clear failed'));
    const view = render(<App />);
    await ready(view);
    await waitFor(() =>
      expect(view.getByRole('button', { name: '选择模型' }).textContent).toContain('Model')
    );
    const input = view.getByRole('textbox', { name: '消息' }) as HTMLTextAreaElement;
    fireEvent.change(input, { target: { value: '/clear ' } });
    fireEvent.keyDown(input, { key: 'Enter' });
    await waitFor(() => expect(clear).toHaveBeenCalled());
    await waitFor(() => expect(input.getAttribute('aria-busy')).toBe('false'));
    expect(input.value).toBe('/clear ');
    expect(mocks.send).not.toHaveBeenCalled();
    expect(view.queryByText('已收到')).toBeNull();
  });
});
