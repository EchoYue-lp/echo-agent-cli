// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.hoisted(() => {
  Object.defineProperty(window, 'matchMedia', {
    configurable: true,
    value: () => ({ matches: false }),
  });
});
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { SchedulerHistory, SchedulerRunLink } from './SchedulerHistory';
import { useWorkspaceStore } from '../../stores/workspaceStore';
import { useConversationStore } from '../../stores/conversationStore';
import { useTaskRuntimeStore, taskRuntimePresentation } from '../../stores/taskRuntimeStore';
import { useUiStore } from '../../stores/uiStore';
import { useContextPaneStore } from '../../stores/contextPaneStore';
import { useWorkspaceViewStore } from '../../stores/workspaceViewStore';
import { useToastStore } from '../../stores/toastStore';
import type { TaskRun } from '../../generated';
import type { Workspace } from '../../api/endpoints';
const history = vi.hoisted(() => vi.fn());
const notificationResult = vi.hoisted(() => vi.fn());
const io = vi.hoisted(() => ({
  conversations: vi.fn(),
  conversation: vi.fn(),
  restore: vi.fn(),
  tools: vi.fn(),
  getRun: vi.fn(),
  getPlan: vi.fn(),
  listTodos: vi.fn(),
  listArtifacts: vi.fn(),
  listRecoveryBlockers: vi.fn(),
  getContinuation: vi.fn(),
  listBackgroundCells: vi.fn(),
  getCompletionGate: vi.fn(),
  listEvents: vi.fn(),
}));
vi.mock('../../api/endpoints', () => ({
  schedulerApi: { history },
  weixinNotificationApi: { result: notificationResult },
  conversationApi: { list: io.conversations, get: io.conversation, restore: io.restore },
  taskRuntimeApi: io,
  toolExecutionApi: { list: io.tools },
}));
afterEach(() => {
  cleanup();
  vi.clearAllMocks();
  vi.restoreAllMocks();
});
it('preserves the original cursor and read facts on a retention failure without retry or reset', async () => {
  const token = '{"after_sequence":18446744073709551614}';
  history
    .mockResolvedValueOnce({
      success: true,
      page: {
        journal_identity: 'journal',
        retained_floor: '1',
        snapshot_end_sequence: '18446744073709551615',
        next_cursor: token,
        events: [
          {
            sequence: '1',
            batch_id: 'batch',
            event: {
              kind: 'persisted',
              envelope: {
                message_id: 'occurrence-original',
                payload: {
                  task: { id: 'task-a', name: '任务 A', definition_id: 'old-definition' },
                },
              },
            },
          },
        ],
      },
      runs: {},
    })
    .mockResolvedValueOnce({
      success: false,
      cause: 'retained_floor_changed',
      retained_floor: '10',
    });
  render(<SchedulerHistory />);
  fireEvent.click(screen.getByRole('button', { name: '读取运行历史' }));
  expect(await screen.findByText(/occurrence-original/)).toBeTruthy();
  expect(screen.getByText(/还有未读页/)).toBeTruthy();
  fireEvent.click(screen.getByRole('button', { name: '加载下一页' }));
  await waitFor(() => expect(history).toHaveBeenLastCalledWith(token));
  expect(await screen.findByRole('alert')).toBeTruthy();
  expect(screen.getByText(/occurrence-original/)).toBeTruthy();
  expect(history).toHaveBeenCalledTimes(2);
  expect(screen.getByText(/尚无可定位的 TaskRun/)).toBeTruthy();
  expect(notificationResult).not.toHaveBeenCalled();
});

it('observes notification status only for the run identified by a loaded history record', async () => {
  history.mockResolvedValue({
    success: true,
    page: {
      journal_identity: 'journal',
      retained_floor: '1',
      snapshot_end_sequence: '1',
      next_cursor: null,
      events: [
        {
          sequence: '1',
          batch_id: 'batch',
          event: {
            kind: 'persisted',
            envelope: {
              message_id: 'occurrence-original',
              payload: {
                task: { id: 'task-a', name: '任务 A', definition_id: 'old-definition' },
              },
            },
          },
        },
      ],
    },
    runs: {
      'occurrence-original': {
        lookup: 'found',
        run: {
          run_id: 'history-run-original',
          workspace_id: 'workspace-original',
          conversation_id: 'conversation-original',
          status: 'completed',
        },
      },
    },
  });
  notificationResult.mockResolvedValue({
    configured: true,
    run_id: 'history-run-original',
    phase: 'turn_settled',
    outcome: 'failed',
    reason: '微信服务拒绝',
  });
  render(<SchedulerHistory />);
  expect(notificationResult).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole('button', { name: '读取运行历史' }));
  expect(await screen.findByText('微信结果通知失败')).toBeTruthy();
  expect(notificationResult).toHaveBeenCalledWith('history-run-original');
  expect(history).toHaveBeenCalledTimes(1);
  expect(screen.queryByText('微信已接受结果通知')).toBeNull();
});

describe('original run navigation with real stores and mocked API IO', () => {
  const original: TaskRun = {
    run_id: 'cron-original-failed',
    workspace_id: 'global',
    conversation_id: 'cron:original-task:cron-scheduled:original-occurrence',
    root_message_id: 'original-occurrence',
    domain_profile: 'general',
    domain_binding: null,
    status: 'failed',
    goal: '原自动任务',
    goal_revision: 1,
    goal_sha256: 'original-goal',
    plan_id: null,
    route: 'agent_task_plan',
    attended_mode: 'unattended',
    created_at: '2026-11-01T03:18:00Z',
    updated_at: '2026-11-01T03:18:01Z',
  };
  const failure = {
    seq: '1',
    run_id: original.run_id,
    task_id: null,
    step_id: null,
    event_type: 'run_failed',
    payload: { error: 'create message log: File name too long (os error 63)' },
    timestamp: original.updated_at,
  };
  const live = { ...original, run_id: 'foreground-original', conversation_id: 'foreground-chat' };
  const workspace: Workspace = {
    id: 'cron-workspace',
    name: '定时任务工作区',
    root: '/product-data/cron',
    project_root: '/project/cron',
    kind: { type: 'code' },
    metadata: { tags: [], project_location: { kind: 'local', path: '/project/cron' } },
    product_data_generation: 'cron-generation-original',
    created_at: '',
    last_active: '',
  };
  function click(run = original) {
    render(<SchedulerRunLink run={run} />);
    fireEvent.click(screen.getByRole('button', { name: `查看任务结果 ${run.run_id}` }));
  }
  function expectNotOpened() {
    expect(useContextPaneStore.getState().target).toBeNull();
    expect(useUiStore.getState().settingsOpen).toBe(true);
    expect(useWorkspaceViewStore.getState().activeView).toBe('analysis');
  }
  function pending<T>() {
    let resolve: (value: T) => void = () => undefined;
    const promise = new Promise<T>((done) => {
      resolve = done;
    });
    return { promise, resolve };
  }

  beforeEach(() => {
    vi.resetAllMocks();
    notificationResult.mockResolvedValue({ configured: false, run_id: original.run_id });
    io.conversations.mockResolvedValue([]);
    io.conversation.mockRejectedValue(new Error('Conversation not found'));
    io.tools.mockResolvedValue([]);
    io.restore.mockResolvedValue({});
    io.getRun.mockResolvedValue(original);
    io.getPlan.mockResolvedValue(null);
    io.listTodos.mockResolvedValue([]);
    io.listArtifacts.mockResolvedValue([]);
    io.listRecoveryBlockers.mockResolvedValue([]);
    io.getContinuation.mockResolvedValue(null);
    io.listBackgroundCells.mockResolvedValue([]);
    io.getCompletionGate.mockResolvedValue({ ready: false, requirements: [], blockers: [] });
    io.listEvents.mockResolvedValue({
      events: [failure],
      next_after_sequence: '1',
      has_more: false,
    });
    useWorkspaceStore.setState({ current: null, workspaces: [] });
    useConversationStore.setState({
      workspaceId: 'global',
      activeId: 'foreground-chat',
      newConversationEpoch: 0,
      sideConversations: [],
    });
    useTaskRuntimeStore.getState().reset();
    useTaskRuntimeStore.setState({
      activeRun: live,
      taskScope: { workspaceId: 'global', workspaceGeneration: 'global' },
    });
    useUiStore.setState({ settingsOpen: true, navigationEpoch: 0 });
    useContextPaneStore.getState().reset();
    useWorkspaceViewStore.getState().open('analysis');
    useToastStore.getState().clearAll();
  });

  it('opens the exact planless failed run without requiring or creating a transcript', async () => {
    click();
    await waitFor(() => expect(io.getRun).toHaveBeenCalled());
    await waitFor(() => expect(useTaskRuntimeStore.getState().selectedRunId).toBe(original.run_id));
    expect(taskRuntimePresentation(useTaskRuntimeStore.getState()).activeRun).toEqual(original);
    expect(taskRuntimePresentation(useTaskRuntimeStore.getState()).plan).toBeNull();
    expect(taskRuntimePresentation(useTaskRuntimeStore.getState()).events).toEqual([failure]);
    expect(useTaskRuntimeStore.getState().activeRun).toEqual(live);
    expect(useConversationStore.getState().activeId).toBe('foreground-chat');
    expect(io.getRun).toHaveBeenCalledExactlyOnceWith('global', original.run_id, 'global');
    expect(io.conversation).not.toHaveBeenCalled();
    expect(io.restore).not.toHaveBeenCalled();
    expect(useToastStore.getState().toasts).toEqual([]);
    expect(useUiStore.getState().settingsOpen).toBe(false);
    expect(useWorkspaceViewStore.getState().activeView).toBe('chat');
    expect(useContextPaneStore.getState().target).toEqual({ kind: 'tasks' });
  });

  it('still loads an existing transcript before selecting the original run, never its latest run', async () => {
    io.conversations.mockResolvedValue([{ conversation_id: original.conversation_id }]);
    io.conversation.mockResolvedValue({
      id: original.conversation_id,
      conversation_id: original.conversation_id,
      title: '原会话',
      messages: [],
      created_at: original.created_at,
      updated_at: original.updated_at,
    });
    click();
    await waitFor(() => expect(useContextPaneStore.getState().target).toEqual({ kind: 'tasks' }));
    expect(io.conversation).toHaveBeenCalledExactlyOnceWith('global', original.conversation_id);
    expect(io.restore).toHaveBeenCalledExactlyOnceWith('global', original.conversation_id);
    expect(useConversationStore.getState().activeId).toBe(original.conversation_id);
    expect(taskRuntimePresentation(useTaskRuntimeStore.getState()).activeRun).toEqual(original);
    expect(io.getRun).toHaveBeenCalledExactlyOnceWith('global', original.run_id, 'global');
  });

  it.each(['网络读取失败', 'permission denied'])(
    'keeps metadata error visible: %s',
    async (message) => {
      io.conversations.mockRejectedValue(new Error(message));
      click();
      await waitFor(() => expect(useToastStore.getState().toasts.at(-1)?.message).toBe(message));
      expect(io.getRun).not.toHaveBeenCalled();
      expect(io.conversation).not.toHaveBeenCalled();
      expectNotOpened();
    }
  );

  it('keeps the original run read failure rather than opening a previous snapshot', async () => {
    io.getRun.mockRejectedValue(new Error('journal permission denied'));
    click();
    await waitFor(() =>
      expect(useToastStore.getState().toasts.at(-1)?.message).toBe('journal permission denied')
    );
    expect(useTaskRuntimeStore.getState().historyError).toBe('journal permission denied');
    expect(useTaskRuntimeStore.getState().activeRun).toEqual(live);
    expectNotOpened();
  });

  it.each(['different', 'same'] as const)(
    'does not treat a failed read of an existing transcript as success with %s activeId',
    async (selection) => {
      const activeId = selection === 'same' ? original.conversation_id : 'foreground-chat';
      useConversationStore.setState({ activeId });
      io.conversations.mockResolvedValue([{ conversation_id: original.conversation_id }]);
      const error = new Error('transcript permission denied');
      io.conversation.mockRejectedValue(error);
      const report = vi.spyOn(console, 'error').mockImplementation(() => undefined);
      click();
      await waitFor(() =>
        expect(report).toHaveBeenCalledWith('Failed to load conversation:', error)
      );
      await act(async () => {});
      expect(useToastStore.getState().toasts).toContainEqual(
        expect.objectContaining({
          type: 'error',
          message: '无法加载会话或工具历史；原始记录保留，请重试',
        })
      );
      expect(useConversationStore.getState().activeId).toBe(activeId);
      expect(useTaskRuntimeStore.getState().activeRun).toEqual(live);
      expect(io.getRun).not.toHaveBeenCalled();
      expect(io.restore).not.toHaveBeenCalled();
      expectNotOpened();
    }
  );

  it.each(['run_id', 'workspace_id', 'conversation_id'] as const)(
    'rejects a mismatched %s',
    async (field) => {
      io.getRun.mockResolvedValue({ ...original, [field]: 'another-identity' });
      click();
      await waitFor(() => expect(useToastStore.getState().toasts.at(-1)?.type).toBe('error'));
      expectNotOpened();
    }
  );

  it('opens a workspace run with its captured generation after the switch is confirmed', async () => {
    const run = { ...original, workspace_id: workspace.id };
    useWorkspaceStore.setState({ workspaces: [workspace] });
    const switchTo = vi
      .spyOn(useWorkspaceStore.getState(), 'switchTo')
      .mockImplementation(async (id) => {
        expect(id).toBe(workspace.id);
        useWorkspaceStore.setState({ current: workspace });
        useConversationStore.setState({ workspaceId: id, newConversationEpoch: 1 });
      });
    io.getRun.mockResolvedValue(run);
    click(run);
    await waitFor(() => expect(useContextPaneStore.getState().target).toEqual({ kind: 'tasks' }));
    expect(switchTo).toHaveBeenCalledExactlyOnceWith(workspace.id);
    expect(io.conversations).toHaveBeenCalledExactlyOnceWith(workspace.id);
    expect(io.getRun).toHaveBeenCalledExactlyOnceWith(
      workspace.id,
      run.run_id,
      workspace.product_data_generation
    );
    expect(taskRuntimePresentation(useTaskRuntimeStore.getState()).activeRun).toEqual(run);
  });

  it('confirms exit to global before reading the global original run', async () => {
    useWorkspaceStore.setState({ current: workspace, workspaces: [workspace] });
    useConversationStore.setState({ workspaceId: workspace.id });
    const exit = vi.spyOn(useWorkspaceStore.getState(), 'exit').mockImplementation(async () => {
      useWorkspaceStore.setState({ current: null });
      useConversationStore.setState({ workspaceId: 'global', newConversationEpoch: 1 });
    });
    click();
    await waitFor(() => expect(useContextPaneStore.getState().target).toEqual({ kind: 'tasks' }));
    expect(exit).toHaveBeenCalledExactlyOnceWith();
    expect(io.getRun).toHaveBeenCalledExactlyOnceWith('global', original.run_id, 'global');
  });

  it.each(['navigation', 'draft', 'generation'] as const)(
    'ignores metadata after a newer %s',
    async (change) => {
      const run = { ...original, workspace_id: workspace.id };
      useWorkspaceStore.setState({ current: workspace, workspaces: [workspace] });
      useConversationStore.setState({ workspaceId: workspace.id });
      const list = pending<[]>();
      io.conversations.mockReturnValue(list.promise);
      click(run);
      await waitFor(() => expect(io.conversations).toHaveBeenCalled());
      await act(async () => {
        if (change === 'navigation') useUiStore.getState().beginNavigation();
        if (change === 'draft') useConversationStore.setState({ newConversationEpoch: 2 });
        if (change === 'generation')
          useWorkspaceStore.setState({
            current: { ...workspace, product_data_generation: 'replacement' },
          });
        list.resolve([]);
      });
      expect(io.getRun).not.toHaveBeenCalled();
      expect(useToastStore.getState().toasts).toEqual([]);
      expectNotOpened();
    }
  );

  it('does not close settings or open a panel after a newer navigation during the run read', async () => {
    const read = pending<TaskRun>();
    io.getRun.mockReturnValue(read.promise);
    click();
    await waitFor(() => expect(io.getRun).toHaveBeenCalled());
    await act(async () => {
      useUiStore.getState().beginNavigation();
      read.resolve(original);
    });
    expectNotOpened();
    expect(useTaskRuntimeStore.getState().activeRun).toEqual(live);
  });
});
