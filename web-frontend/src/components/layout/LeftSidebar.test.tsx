// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { Workspace } from '../../api/endpoints';
import type { ConversationListItem } from '../../types/api';
import type { TaskRun } from '../../generated';

const mocks = vi.hoisted(() => ({
  list: vi.fn(),
  listRuns: vi.fn(),
  loadByRun: vi.fn(),
  openTasks: vi.fn(),
  search: vi.fn(),
  switchTo: vi.fn(),
  load: vi.fn(),
  toast: vi.fn(),
  openChat: vi.fn(),
  ui: {
    navigationEpoch: 0,
    theme: 'light',
    toggleTheme: vi.fn(),
    openSettings: vi.fn(),
    setActiveSettingsTab: vi.fn(),
    toggleTerminal: vi.fn(),
    openWorkspaceDialog: vi.fn(),
    beginNavigation: (): number => 0,
  },
  workspace: {
    workspaces: [] as Workspace[],
    current: null as Workspace | null,
    isLoading: false,
    switchTo: vi.fn(),
    delete: vi.fn(),
    exit: vi.fn(),
  },
  conversation: {
    workspaceId: 'global',
    conversations: [],
    sideConversations: [],
    activeId: null,
    isLoading: false,
    newConversationEpoch: 0,
    loadConversation: vi.fn(),
    startNew: vi.fn(),
    archiveConversation: vi.fn(),
    deleteConversation: vi.fn(),
    renameConversation: vi.fn(),
    retrySideConversation: vi.fn(),
  },
}));
vi.mock('../../stores/uiStore', () => ({
  useUiStore: Object.assign((select: (state: typeof mocks.ui) => unknown) => select(mocks.ui), {
    getState: () => mocks.ui,
  }),
}));
vi.mock('../../stores/workspaceStore', () => ({
  useWorkspaceStore: Object.assign(
    (select: (state: typeof mocks.workspace) => unknown) => select(mocks.workspace),
    { getState: () => mocks.workspace }
  ),
}));
vi.mock('../../stores/conversationStore', () => ({
  useConversationStore: Object.assign(
    (select: (state: typeof mocks.conversation) => unknown) => select(mocks.conversation),
    { getState: () => mocks.conversation }
  ),
}));
vi.mock('../../stores/toastStore', () => ({
  useToastStore: { getState: () => ({ addToast: mocks.toast }) },
}));
vi.mock('../../stores/workspaceViewStore', () => ({
  useWorkspaceViewStore: { getState: () => ({ openChat: mocks.openChat }) },
}));
vi.mock('../../api/endpoints', () => ({
  conversationApi: { list: mocks.list, search: mocks.search },
  taskRuntimeApi: { listRuns: mocks.listRuns },
}));
vi.mock('../../stores/taskRuntimeStore', () => ({
  useTaskRuntimeStore: { getState: () => ({ loadByRun: mocks.loadByRun }) },
}));
vi.mock('../../stores/contextPaneStore', () => ({
  useContextPaneStore: { getState: () => ({ openTasks: mocks.openTasks }) },
}));
vi.mock('../../lib/tauri-bridge', () => ({ fileSystem: { openPath: vi.fn() } }));
import { LeftSidebar } from './LeftSidebar';

const workspace = (id: string): Workspace => ({
  id,
  name: id,
  root: `/product-data/${id}`,
  project_root: `/code/${id}`,
  kind: { type: 'code' },
  metadata: { tags: [], project_location: { kind: 'local' as const, path: `/code/${id}` } },
  product_data_generation: `${id}-v1`,
  created_at: '',
  last_active: id,
});
const task = (id: string, extra: Partial<ConversationListItem> = {}): ConversationListItem => ({
  id: 1,
  conversation_id: id,
  title: id,
  message_count: 1,
  created_at: '2026-10-25',
  updated_at: '2026-10-25',
  ...extra,
});
function deferred<T>() {
  let resolve: (value: T) => void = () => undefined;
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}
afterEach(cleanup);
beforeEach(() => {
  vi.clearAllMocks();
  const a = workspace('a');
  const b = workspace('b');
  mocks.workspace.workspaces = [a, b];
  mocks.workspace.current = a;
  mocks.workspace.isLoading = false;
  mocks.conversation.workspaceId = 'a';
  mocks.conversation.newConversationEpoch = 0;
  mocks.ui.navigationEpoch = 0;
  mocks.ui.beginNavigation = () => ++mocks.ui.navigationEpoch;
  mocks.conversation.loadConversation = mocks.load;
  mocks.workspace.switchTo = mocks.switchTo;
  mocks.list.mockImplementation(async (id: string) => (id === 'b' ? [task('remote-task')] : []));
  mocks.switchTo.mockImplementation(async (id: string) => {
    mocks.conversation.newConversationEpoch += 1;
    mocks.workspace.current = mocks.workspace.workspaces.find((item) => item.id === id) ?? null;
    mocks.conversation.workspaceId = id;
  });
  mocks.load.mockResolvedValue(undefined);
  mocks.listRuns.mockReset();
  mocks.listRuns.mockResolvedValue({ items: [], next_continuation: null });
  mocks.loadByRun.mockResolvedValue(undefined);
  mocks.search.mockReset();
  mocks.search.mockResolvedValue([]);
});

describe('task and workspace navigation', () => {
  const run = (id: string, conversationId: string): TaskRun => ({
    run_id: id,
    workspace_id: 'a',
    conversation_id: conversationId,
    root_message_id: `root-${id}`,
    domain_profile: 'general',
    domain_binding: null,
    status: 'failed',
    goal: `运行 ${id}`,
    goal_revision: 1,
    goal_sha256: `goal-${id}`,
    plan_id: null,
    route: 'agent_task_plan',
    attended_mode: 'attended',
    created_at: '2026-10-31T15:17:00Z',
    updated_at: '2026-10-31T15:17:20Z',
  });
  it('shows persisted IM and cron runs even when no GUI conversation has been opened', async () => {
    mocks.list.mockResolvedValue([]);
    mocks.listRuns.mockImplementation(async (scope: { workspaceId: string }) => ({
      items:
        scope.workspaceId === 'a'
          ? [run('im', 'channel:sha256:one'), run('cron', 'cron:daily:slot-1')]
          : [],
      next_continuation: null,
    }));
    const view = render(<LeftSidebar onNewTask={vi.fn()} />);
    expect(await view.findByRole('button', { name: '打开任务 运行 im，a' })).toBeTruthy();
    expect(view.getByRole('button', { name: '打开任务 运行 cron，a' })).toBeTruthy();
    expect(view.getByText('IM · 失败')).toBeTruthy();
    expect(view.getByText('定时 · 失败')).toBeTruthy();
    fireEvent.click(view.getByRole('button', { name: '打开任务 运行 im，a' }));
    await waitFor(() =>
      expect(mocks.loadByRun).toHaveBeenCalledWith(
        { workspaceId: 'a', workspaceGeneration: 'a-v1' },
        'im'
      )
    );
    expect(mocks.openTasks).toHaveBeenCalled();
    expect(mocks.load).not.toHaveBeenCalled();
  });
  it('refreshes background history on focus without a foreground conversation mutation', async () => {
    mocks.list.mockResolvedValue([]);
    const view = render(<LeftSidebar onNewTask={vi.fn()} />);
    await waitFor(() => expect(view.getByText('从一个新任务开始')).toBeTruthy());
    mocks.listRuns.mockImplementation(async (scope: { workspaceId: string }) => ({
      items: scope.workspaceId === 'a' ? [run('later', 'channel:sha256:later')] : [],
      next_continuation: null,
    }));
    fireEvent.focus(window);
    expect(await view.findByRole('button', { name: '打开任务 运行 later，a' })).toBeTruthy();
  });
  it('keeps each background run once and retains ordinary GUI conversation rows', async () => {
    mocks.list.mockImplementation(async (id: string) =>
      id === 'a'
        ? [task('channel:sha256:shared', { title: '不重复显示此会话行' }), task('ordinary-gui')]
        : []
    );
    const first = run('first', 'channel:sha256:shared');
    const second = { ...run('second', 'channel:sha256:shared'), status: 'cancelled' as const };
    mocks.listRuns.mockImplementation(async (scope: { workspaceId: string }) => ({
      items:
        scope.workspaceId === 'a'
          ? [first, first, second, run('ordinary-run', 'ordinary-gui')]
          : [],
      next_continuation: null,
    }));
    const view = render(<LeftSidebar onNewTask={vi.fn()} />);
    expect(await view.findAllByRole('button', { name: '打开任务 运行 first，a' })).toHaveLength(1);
    expect(view.getAllByRole('button', { name: '打开任务 运行 second，a' })).toHaveLength(1);
    expect(view.getByText('IM · 已取消')).toBeTruthy();
    expect(view.queryByText('不重复显示此会话行')).toBeNull();
    expect(view.getByRole('button', { name: '打开任务 ordinary-gui，a' })).toBeTruthy();
    expect(view.queryByText('运行 ordinary-run')).toBeNull();
    fireEvent.click(view.getByRole('button', { name: '打开任务 运行 second，a' }));
    await waitFor(() =>
      expect(mocks.loadByRun).toHaveBeenCalledWith(
        { workspaceId: 'a', workspaceGeneration: 'a-v1' },
        'second'
      )
    );
    expect(mocks.load).toHaveBeenCalledExactlyOnceWith('channel:sha256:shared');
  });
  it('rejects a delayed run page after its workspace generation has changed', async () => {
    mocks.list.mockResolvedValue([]);
    const pending = deferred<{ items: TaskRun[]; next_continuation: string | null }>();
    mocks.listRuns.mockImplementation(
      (scope: { workspaceId: string; workspaceGeneration: string }) =>
        scope.workspaceId === 'b' && scope.workspaceGeneration === 'b-v1'
          ? pending.promise
          : Promise.resolve({ items: [], next_continuation: null })
    );
    const view = render(<LeftSidebar onNewTask={vi.fn()} />);
    await waitFor(() =>
      expect(mocks.listRuns).toHaveBeenCalledWith(
        { workspaceId: 'b', workspaceGeneration: 'b-v1' },
        [],
        null,
        8
      )
    );
    mocks.workspace.workspaces = [
      workspace('a'),
      { ...workspace('b'), product_data_generation: 'b-v2' },
    ];
    await act(async () =>
      pending.resolve({
        items: [{ ...run('stale', 'cron:daily:slot'), workspace_id: 'b' }],
        next_continuation: null,
      })
    );
    expect(view.queryByRole('button', { name: '打开任务 运行 stale，b' })).toBeNull();
    expect(mocks.loadByRun).not.toHaveBeenCalled();
  });
  it('reads an older finite run page only after the user asks for more', async () => {
    mocks.list.mockResolvedValue([]);
    mocks.listRuns.mockImplementation(
      async (scope: { workspaceId: string }, _statuses, cursor) => ({
        items:
          scope.workspaceId !== 'a' ? [] : [run(cursor ? 'older' : 'latest', 'cron:daily:slot')],
        next_continuation: scope.workspaceId === 'a' && !cursor ? 'older-page' : null,
      })
    );
    const view = render(<LeftSidebar onNewTask={vi.fn()} />);
    expect(await view.findByRole('button', { name: '打开任务 运行 latest，a' })).toBeTruthy();
    expect(mocks.listRuns).not.toHaveBeenCalledWith(
      { workspaceId: 'a', workspaceGeneration: 'a-v1' },
      [],
      'older-page',
      8
    );
    fireEvent.click(view.getByRole('button', { name: '查看更多任务' }));
    expect(await view.findByRole('button', { name: '打开任务 运行 older，a' })).toBeTruthy();
    expect(view.getAllByRole('button', { name: '打开任务 运行 latest，a' })).toHaveLength(1);
    expect(mocks.listRuns).toHaveBeenCalledWith(
      { workspaceId: 'a', workspaceGeneration: 'a-v1' },
      [],
      'older-page',
      8
    );
  });
  it('stops hidden polling and refreshes immediately when the page becomes visible', async () => {
    mocks.list.mockResolvedValue([]);
    const view = render(<LeftSidebar onNewTask={vi.fn()} />);
    await waitFor(() => expect(view.getByText('从一个新任务开始')).toBeTruthy());
    const calls = mocks.listRuns.mock.calls.length;
    const oldHidden = Object.getOwnPropertyDescriptor(document, 'hidden');
    try {
      Object.defineProperty(document, 'hidden', { configurable: true, value: true });
      fireEvent(document, new Event('visibilitychange'));
      vi.useFakeTimers();
      await act(async () => vi.advanceTimersByTime(15000));
      expect(mocks.listRuns).toHaveBeenCalledTimes(calls);
      Object.defineProperty(document, 'hidden', { configurable: true, value: false });
      await act(async () => fireEvent(document, new Event('visibilitychange')));
      expect(mocks.listRuns.mock.calls.length).toBeGreaterThan(calls);
    } finally {
      vi.useRealTimers();
      if (oldHidden) Object.defineProperty(document, 'hidden', oldHidden);
      else Reflect.deleteProperty(document, 'hidden');
    }
  });
  it('deduplicates a retained conversation after partial metadata failure and a new run', async () => {
    const conversationId = 'channel:sha256:partial';
    mocks.list.mockImplementation(async (id: string) =>
      id === 'a'
        ? [task(conversationId, { title: '旧IM会话' }), task('ordinary-retained')]
        : id === 'b'
          ? [task(conversationId, { title: '另一工作空间的同ID会话' })]
          : []
    );
    const view = render(<LeftSidebar onNewTask={vi.fn()} />);
    expect(await view.findByRole('button', { name: '打开任务 旧IM会话，a' })).toBeTruthy();
    mocks.list.mockImplementation(async (id: string) => {
      if (id === 'a') throw new Error('metadata temporarily unavailable');
      return id === 'b' ? [task(conversationId, { title: '另一工作空间的同ID会话' })] : [];
    });
    mocks.listRuns.mockImplementation(async (scope: { workspaceId: string }) => ({
      items: scope.workspaceId === 'a' ? [run('new-background', conversationId)] : [],
      next_continuation: null,
    }));
    fireEvent.focus(window);
    expect(
      await view.findByRole('button', { name: '打开任务 运行 new-background，a' })
    ).toBeTruthy();
    expect(view.queryByRole('button', { name: '打开任务 旧IM会话，a' })).toBeNull();
    expect(view.getByRole('button', { name: '打开任务 ordinary-retained，a' })).toBeTruthy();
    expect(view.getByRole('button', { name: '打开任务 另一工作空间的同ID会话，b' })).toBeTruthy();
    expect(view.getByText('1 个工作空间的最近任务暂时无法读取')).toBeTruthy();
  });
  it('keeps new-task distinct from adding a workspace and preserves service discovery', async () => {
    const start = vi.fn();
    const view = render(<LeftSidebar onNewTask={start} />);
    fireEvent.click(view.getByRole('button', { name: '新建任务' }));
    expect(start).toHaveBeenCalledOnce();
    expect(mocks.ui.openWorkspaceDialog).not.toHaveBeenCalled();
    fireEvent.click(view.getByRole('button', { name: '添加工作空间' }));
    expect(mocks.ui.openWorkspaceDialog).toHaveBeenCalledOnce();
    fireEvent.click(view.getByRole('button', { name: 'IM 渠道' }));
    expect(mocks.ui.setActiveSettingsTab).toHaveBeenLastCalledWith('channels');
    fireEvent.click(view.getByRole('button', { name: '定时任务' }));
    expect(mocks.ui.setActiveSettingsTab).toHaveBeenLastCalledWith('scheduler');
    await waitFor(() =>
      expect(view.getByRole('button', { name: '打开任务 remote-task，b' })).toBeTruthy()
    );
  });
  it('does not accept a new draft while the original workspace transition is unsettled', async () => {
    mocks.workspace.isLoading = true;
    const start = vi.fn();
    const view = render(<LeftSidebar onNewTask={start} />);
    const button = view.getByRole('button', { name: '新建任务' }) as HTMLButtonElement;
    expect(button.disabled).toBe(true);
    fireEvent.click(button);
    expect(start).not.toHaveBeenCalled();
    await waitFor(() => expect(mocks.list).toHaveBeenCalled());
  });
  it('waits for the original workspace transition before loading the task', async () => {
    const transition = deferred<void>();
    mocks.switchTo.mockImplementation((id: string) => {
      mocks.conversation.newConversationEpoch += 1;
      return transition.promise.then(() => {
        mocks.workspace.current = mocks.workspace.workspaces.find((item) => item.id === id) ?? null;
        mocks.conversation.workspaceId = id;
      });
    });
    const view = render(<LeftSidebar onNewTask={vi.fn()} />);
    fireEvent.click(await view.findByRole('button', { name: '打开任务 remote-task，b' }));
    expect(mocks.switchTo).toHaveBeenCalledWith('b');
    expect(mocks.load).not.toHaveBeenCalled();
    await act(async () => transition.resolve());
    expect(mocks.load).toHaveBeenCalledExactlyOnceWith('remote-task');
  });
  it('does not load the old task after a newer new-task intent', async () => {
    const transition = deferred<void>();
    mocks.switchTo.mockImplementation(() => {
      mocks.conversation.newConversationEpoch += 1;
      return transition.promise.then(() => {
        mocks.workspace.current = mocks.workspace.workspaces[1] ?? null;
        mocks.conversation.workspaceId = 'b';
      });
    });
    const view = render(
      <LeftSidebar
        onNewTask={() => {
          mocks.ui.beginNavigation();
          mocks.conversation.newConversationEpoch += 1;
        }}
      />
    );
    fireEvent.click(await view.findByRole('button', { name: '打开任务 remote-task，b' }));
    fireEvent.click(view.getByRole('button', { name: '新建任务' }));
    await act(async () => transition.resolve());
    expect(mocks.load).not.toHaveBeenCalled();
    expect(mocks.openChat).not.toHaveBeenCalled();
  });
  it('rejects a row whose workspace id was reused with another generation', async () => {
    const view = render(<LeftSidebar onNewTask={vi.fn()} />);
    const row = await view.findByRole('button', { name: '打开任务 remote-task，b' });
    const replacement = workspace('b');
    replacement.product_data_generation = 'b-v2';
    mocks.workspace.workspaces = [workspace('a'), replacement];
    fireEvent.click(row);
    expect(mocks.switchTo).not.toHaveBeenCalled();
    expect(mocks.load).not.toHaveBeenCalled();
    expect(mocks.toast).toHaveBeenCalled();
  });
  it('excludes archived and side conversations from the recent task projection', async () => {
    mocks.list.mockResolvedValue([
      task('ordinary'),
      task('archived', { archived: true }),
      task('side', {
        side_conversation: {} as NonNullable<ConversationListItem['side_conversation']>,
      }),
    ]);
    const view = render(<LeftSidebar onNewTask={vi.fn()} />);
    await waitFor(() => expect(view.getAllByText('ordinary')).toHaveLength(3));
    expect(view.queryByText('archived')).toBeNull();
    expect(view.queryByText('side')).toBeNull();
  });
  it('bounds initial reads to six scopes and at most two concurrent metadata calls', async () => {
    mocks.workspace.workspaces = Array.from({ length: 20 }, (_, index) => workspace(`w${index}`));
    const pending: Array<ReturnType<typeof deferred<ConversationListItem[]>>> = [];
    let active = 0;
    let maximum = 0;
    mocks.list.mockImplementation(() => {
      active += 1;
      maximum = Math.max(maximum, active);
      const entry = deferred<ConversationListItem[]>();
      pending.push(entry);
      return entry.promise.finally(() => {
        active -= 1;
      });
    });
    const view = render(<LeftSidebar onNewTask={vi.fn()} />);
    expect(mocks.list).toHaveBeenCalledTimes(2);
    for (let index = 0; index < 3; index += 1)
      await act(async () => {
        for (const entry of pending.splice(0)) entry.resolve([]);
      });
    await waitFor(() => expect(mocks.list).toHaveBeenCalledTimes(6));
    expect(maximum).toBe(2);
    expect(view.getByRole('button', { name: '查看更多任务' })).toBeTruthy();
  });
  it.each(['different workspace', 'same id new generation'])(
    'rejects already-visible search rows after %s, while B search is pending',
    async (replacement) => {
      mocks.search
        .mockResolvedValueOnce([task('search-result')])
        .mockReturnValueOnce(new Promise(() => undefined));
      const view = render(<LeftSidebar onNewTask={vi.fn()} />);
      fireEvent.change(view.getByRole('textbox', { name: '搜索任务或工作空间' }), {
        target: { value: 'search' },
      });
      const label = await view.findByText('search-result');
      const row = label.closest('[role="button"]');
      if (!row) throw new Error('Search row missing');
      fireEvent.click(row);
      await waitFor(() => expect(mocks.load).toHaveBeenCalledWith('search-result'));
      mocks.load.mockClear();
      const next = workspace(replacement === 'different workspace' ? 'b' : 'a');
      if (replacement === 'same id new generation') next.product_data_generation = 'a-v2';
      mocks.workspace.current = next;
      // Simulate a live scope update before React commits the next rendering.
      fireEvent.keyDown(row, { key: 'Enter' });
      expect(mocks.load).not.toHaveBeenCalled();
      view.rerender(<LeftSidebar onNewTask={vi.fn()} />);
      expect(view.queryByText('search-result')).toBeNull();
    }
  );
});
