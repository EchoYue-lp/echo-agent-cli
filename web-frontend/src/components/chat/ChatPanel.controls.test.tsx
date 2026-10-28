// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { QueuedChatInput } from '../../hooks/useTauriChat';
import type { TaskRun, TaskRunControlReceipt, BackgroundCellState } from '../../generated';
import type { Workspace } from '../../api/endpoints';

const mocks = vi.hoisted(() => {
  Object.defineProperty(window, 'matchMedia', {
    configurable: true,
    value: () => ({ matches: false }),
  });
  return {
    queue: [] as QueuedChatInput[],
    renderRealBubble: false,
    replace: vi.fn(),
    remove: vi.fn(),
    reorder: vi.fn(),
    steer: vi.fn(),
    update: vi.fn(),
    pause: vi.fn(),
    cancel: vi.fn(),
    refresh: vi.fn(),
  };
});
vi.mock('../../hooks/useTauriChat', () => ({
  useTauriChat: () => ({
    queuedInputs: mocks.queue,
    sendMessage: vi.fn(),
    sendApproval: vi.fn(),
    sendInput: vi.fn(),
    sendSelection: vi.fn(),
    cancel: vi.fn(),
    clearQueuedMessages: vi.fn(),
    replaceQueuedMessage: mocks.replace,
    removeQueuedMessage: mocks.remove,
    reorderQueuedMessage: mocks.reorder,
    steerQueuedMessage: mocks.steer,
  }),
}));
// Keep the real ChatPanel/AgentPane/footer and controls; unrelated surfaces do
// not take part in these TaskRuntime and durable-input presentation contracts.
vi.mock('./ChatInput', () => ({ ChatInput: () => <div aria-label="测试输入框" /> }));
vi.mock('./MessageBubble', async (importOriginal) => {
  const actual = await importOriginal<typeof import('./MessageBubble')>();
  return {
    MessageBubble: (props: Parameters<typeof actual.MessageBubble>[0]) =>
      mocks.renderRealBubble ? <actual.MessageBubble {...props} /> : null,
  };
});
vi.mock('./HitlDialog', () => ({ HitlDialog: () => null }));
vi.mock('./WelcomeScreen', () => ({ WelcomeScreen: () => null }));
vi.mock('./FailureToast', () => ({ FailureToast: () => null }));

import { ChatPanel } from './ChatPanel';
import { useTaskRuntimeStore } from '../../stores/taskRuntimeStore';
import { useWorkspaceStore } from '../../stores/workspaceStore';
import { useConversationStore } from '../../stores/conversationStore';
import { useChatStore } from '../../stores/chatStore';
import { useSubagentRunStore } from '../../stores/subagentRunStore';

const originalActions = useTaskRuntimeStore.getState();
const workspace: Workspace = {
  id: 'workspace-a',
  name: 'A',
  root: '/data/a',
  project_root: '/code/a',
  kind: { type: 'code' },
  metadata: { tags: [] },
  product_data_generation: 'generation-a',
  created_at: '',
  last_active: '',
};
const run: TaskRun = {
  run_id: 'run-a',
  workspace_id: workspace.id,
  conversation_id: 'conversation-a',
  root_message_id: 'message-a',
  domain_profile: 'general',
  domain_binding: null,
  goal: '原目标 中文 🧭',
  goal_revision: 1,
  goal_sha256: 'goal-a',
  status: 'paused',
  plan_id: 'plan-a',
  route: 'diagnostic',
  attended_mode: 'attended',
  created_at: '2026-10-26T00:00:00Z',
  updated_at: '2026-10-26T00:00:00Z',
};
function deferred<T>() {
  let resolve: (value: T) => void = () => undefined;
  let reject: (reason: Error) => void = () => undefined;
  const promise = new Promise<T>((done, fail) => {
    resolve = done;
    reject = fail;
  });
  return { promise, resolve, reject };
}
function queueItem(id: string, text = `Steer ${id}`): QueuedChatInput {
  return {
    id,
    text,
    workspaceId: workspace.id,
    conversationId: run.conversation_id,
    attachments: [],
    phase: 'persisted',
    drained: false,
    identity: {
      address: { workspace_id: workspace.id, conversation_id: run.conversation_id },
      input_id: id,
      revision: 1,
      payload_sha256: `sha-${id}`,
    },
  };
}
function liveRun() {
  return useTaskRuntimeStore.getState().activeRun;
}
function updateRun(patch: Partial<TaskRun>) {
  useTaskRuntimeStore.setState({ activeRun: { ...(liveRun() ?? run), ...patch }, error: null });
}
function beginEdit(text = '新目标 中文 🚀') {
  fireEvent.click(screen.getByRole('button', { name: '编辑当前目标' }));
  fireEvent.change(screen.getByRole('textbox', { name: '编辑当前目标' }), {
    target: { value: text },
  });
  fireEvent.click(screen.getByRole('button', { name: /^保存$/ }));
}
function cell(phase: BackgroundCellState['phase']): BackgroundCellState {
  return {
    cell_id: 'cell-a',
    name: 'work',
    command_hash: 'hash',
    turn_id: null,
    execution_id: null,
    call_id: null,
    phase,
    terminal_cause: null,
    terminal_message: null,
    exit_code: null,
    artifact_status: 'not_requested',
    artifact_message: null,
    total_output_bytes: 0,
    output_truncated: false,
    output_excerpt: null,
    artifact_path: null,
    artifact_sha256: null,
    started_at: '',
    finished_at: null,
  };
}

beforeEach(() => {
  vi.resetAllMocks();
  mocks.queue = [];
  mocks.renderRealBubble = false;
  useSubagentRunStore.getState().clear();
  useTaskRuntimeStore.getState().stopPolling();
  useWorkspaceStore.setState({ current: workspace, isLoading: false });
  useConversationStore.setState({
    workspaceId: workspace.id,
    activeId: run.conversation_id,
    newConversationEpoch: 1,
    isLoading: false,
    conversations: [],
    sideConversations: [],
  });
  useChatStore.setState({
    messages: [],
    pendingHitlRequests: [],
    isStreaming: false,
    isCancelled: false,
    runStatus: 'idle',
  });
  useTaskRuntimeStore.setState({
    activeRun: run,
    taskScope: {
      workspaceId: workspace.id,
      workspaceGeneration: workspace.product_data_generation,
    },
    todos: [],
    error: null,
    continuation: null,
    backgroundCells: [],
    historySnapshot: null,
    selectedRunId: null,
    updateGoal: mocks.update,
    pause: mocks.pause,
    cancel: mocks.cancel,
    refresh: mocks.refresh,
  });
  mocks.refresh.mockResolvedValue(undefined);
  mocks.update.mockImplementation(async (_target, revision: number, goal: string) => {
    updateRun({ goal, goal_revision: revision + 1, goal_sha256: 'updated-goal' });
    return true;
  });
  mocks.pause.mockImplementation(async () => {
    updateRun({ status: 'paused' });
    return { success: true, run_id: run.run_id };
  });
  mocks.cancel.mockResolvedValue({ success: true, run_id: run.run_id });
  mocks.replace.mockResolvedValue(true);
  mocks.steer.mockResolvedValue(true);
  Object.defineProperty(HTMLElement.prototype, 'scrollIntoView', {
    configurable: true,
    value: vi.fn(),
  });
});
afterEach(() => {
  cleanup();
  useTaskRuntimeStore.setState({
    updateGoal: originalActions.updateGoal,
    pause: originalActions.pause,
    cancel: originalActions.cancel,
    refresh: originalActions.refresh,
  });
  vi.useRealTimers();
});

describe('主交互页入口', () => {
  it('已选中对话时也不提供顶部新建支线入口', () => {
    render(<ChatPanel />);
    expect(screen.queryByRole('button', { name: '新建支线对话' })).toBeNull();
    expect(screen.getByLabelText('测试输入框')).toBeTruthy();
    expect(screen.getByRole('button', { name: '编辑当前目标' })).toBeTruthy();
  });
});

describe('真实流式消息与等待状态', () => {
  it('已有真实操作状态时不叠加第二条空白等待提示', () => {
    mocks.renderRealBubble = true;
    useChatStore.getState().startAssistantMessage('operation-waiting');
    useChatStore.getState().setRunStatus('thinking');
    useChatStore.getState().observeActivity('request:one', '模型正在响应');
    const { container } = render(<ChatPanel />);
    expect(screen.getByLabelText('当前执行状态')).toBeTruthy();
    expect(screen.getByText('模型正在响应')).toBeTruthy();
    expect(container.querySelectorAll('.spinner')).toHaveLength(0);
  });

  it('从空思考段、空白增量到首个可见内容始终保留且只保留一处等待状态', () => {
    mocks.renderRealBubble = true;
    useChatStore.setState({
      messages: [{ id: 'stream-user', role: 'user', content: '用户问题', timestamp: 1 }],
    });
    const id = useChatStore.getState().startAssistantMessage('stream-thinking');
    const { container } = render(<ChatPanel />);
    expect(container.querySelectorAll('.spinner')).toHaveLength(1);

    act(() => useChatStore.getState().startThinkingSegment(id));
    expect(useChatStore.getState().messages.at(-1)?.thinkingSegments).toEqual([{ content: '' }]);
    expect(
      container.querySelectorAll('.spinner'),
      '真实 startThinkingSegment 只有空段时仍应显示等待'
    ).toHaveLength(1);
    expect(screen.queryByRole('button', { name: '折叠思考' })).toBeNull();

    act(() => useChatStore.getState().appendThinking(id, ' \n\t '));
    expect(container.querySelectorAll('.spinner')).toHaveLength(1);
    act(() => useChatStore.getState().appendThinking(id, '正在分析 中文 🧭'));
    expect(screen.getByText('正在分析 中文 🧭')).toBeTruthy();
    expect(container.querySelectorAll('.spinner')).toHaveLength(0);

    act(() => useChatStore.getState().startThinkingSegment(id));
    expect(container.querySelectorAll('.spinner')).toHaveLength(0);
    act(() => useChatStore.getState().appendToken(id, '正文首段 中文'));
    expect(screen.getByText('正文首段 中文')).toBeTruthy();
    expect(container.querySelectorAll('.spinner')).toHaveLength(0);
    act(() => useChatStore.getState().applyFinalAnswer(id, '完整回复 中文'));
    act(() => useChatStore.getState().settleAssistantMessage(id));
    expect(screen.getByText('完整回复 中文')).toBeTruthy();
    expect(screen.getByRole('button', { name: '分叉会话' })).toBeTruthy();
    expect(container.querySelectorAll('.spinner')).toHaveLength(0);
  });

  it('offers one fork only after the runtime confirms a final reply', () => {
    mocks.renderRealBubble = true;
    useConversationStore.setState({ activeId: 'source-runtime-final' });
    useChatStore.setState({
      isStreaming: false,
      messages: [
        { id: 'u-final', role: 'user', content: '用户问题', timestamp: 1 },
        {
          id: 'progress-final',
          role: 'assistant',
          content: '公开进度',
          timestamp: 2,
        },
        {
          id: 'reply-final',
          role: 'assistant',
          content: '尚未确认的回答',
          timestamp: 3,
        },
      ],
    });
    render(<ChatPanel />);
    expect(screen.queryByRole('button', { name: '分叉会话' })).toBeNull();
    act(() => useChatStore.getState().applyFinalAnswer('reply-final', '已通过原运行时收尾的回答'));
    expect(screen.getAllByRole('button', { name: '分叉会话' })).toHaveLength(1);
  });

  it('仅有空白正文时继续等待，首个正文字符到达后由真实气泡承接', () => {
    mocks.renderRealBubble = true;
    const id = useChatStore.getState().startAssistantMessage('stream-text');
    const { container } = render(<ChatPanel />);
    act(() => useChatStore.getState().appendToken(id, ' \n\t '));
    expect(container.querySelectorAll('.spinner'), '空白正文被气泡隐藏时仍应显示等待').toHaveLength(
      1
    );
    expect(screen.queryByRole('button', { name: '复制' })).toBeNull();
    act(() => useChatStore.getState().appendToken(id, '正文 中文 🧭'));
    expect(screen.getByText('正文 中文 🧭')).toBeTruthy();
    expect(container.querySelectorAll('.spinner')).toHaveLength(0);
    act(() => useChatStore.getState().startThinkingSegment(id));
    expect(container.querySelectorAll('.spinner')).toHaveLength(0);
  });
});

describe('composer Goal controls', () => {
  it('saves through the original revision/CAS then verifies the canonical projection', async () => {
    render(<ChatPanel />);
    beginEdit();
    await waitFor(() => expect(screen.getByText(/目标已更新；/)).toBeTruthy());
    expect(mocks.update).toHaveBeenCalledWith(
      {
        workspace_id: workspace.id,
        run_id: run.run_id,
        workspace_generation: workspace.product_data_generation,
      },
      1,
      '新目标 中文 🚀',
      '用户更新目标'
    );
    expect(mocks.refresh).toHaveBeenCalledWith(workspace.id, run.run_id);
    expect(screen.queryByRole('textbox', { name: '编辑当前目标' })).toBeNull();
    expect(screen.getByText('Goal r2')).toBeTruthy();
    expect(screen.getByText(/旧任务计划与证据已失效/)).toBeTruthy();
    expect(mocks.pause).not.toHaveBeenCalled();
  });

  it.each(['false', 'throw'] as const)(
    'keeps the draft and reports a %s update failure',
    async (failure) => {
      if (failure === 'false') mocks.update.mockResolvedValue(false);
      else mocks.update.mockRejectedValue(new Error('CAS revision conflict'));
      render(<ChatPanel />);
      beginEdit('保留此草稿');
      await waitFor(() => expect(screen.getByRole('alert')).toBeTruthy());
      expect(
        (screen.getByRole('textbox', { name: '编辑当前目标' }) as HTMLInputElement).value
      ).toBe('保留此草稿');
      expect(screen.queryByText(/目标已更新；/)).toBeNull();
      expect(liveRun()?.goal_revision).toBe(1);
    }
  );

  it('does not call a successful receipt canonical until the updated record is readable', async () => {
    mocks.update.mockResolvedValue(true);
    render(<ChatPanel />);
    beginEdit();
    await waitFor(() => expect(screen.getByRole('alert').textContent).toContain('记录尚未确认'));
    expect(screen.getByRole('textbox', { name: '编辑当前目标' })).toBeTruthy();
    expect(screen.queryByText(/目标已更新；/)).toBeNull();
  });

  it('waits for pause and visible command work before sending the same captured CAS', async () => {
    updateRun({ status: 'running' });
    const pause = deferred<TaskRunControlReceipt>();
    mocks.pause.mockReturnValue(pause.promise);
    let observations = 0;
    mocks.refresh.mockImplementation(async () => {
      observations += 1;
      if (observations === 1) {
        updateRun({ status: 'paused' });
        useTaskRuntimeStore.setState({ backgroundCells: [cell('running')] });
      } else useTaskRuntimeStore.setState({ backgroundCells: [] });
    });
    render(<ChatPanel />);
    beginEdit('运行中修改');
    expect(screen.getByRole('status').textContent).toContain('正在请求暂停');
    expect(
      (screen.getByRole('textbox', { name: '编辑当前目标' }) as HTMLInputElement).disabled
    ).toBe(true);
    expect(mocks.update).not.toHaveBeenCalled();
    await act(async () => pause.resolve({ success: true, run_id: run.run_id }));
    expect(screen.getByRole('status').textContent).toContain('等待任务安全点');
    expect(mocks.update).not.toHaveBeenCalled();
    await waitFor(() => expect(mocks.update).toHaveBeenCalledTimes(1));
    await waitFor(() => expect(screen.getByText(/目标已更新；/)).toBeTruthy());
    expect(mocks.pause).toHaveBeenCalledTimes(1);
  });

  it('re-observes an explicit active-driver CAS rejection before a later accepted update', async () => {
    const accepted = async (_target: unknown, revision: number, goal: string) => {
      updateRun({ goal, goal_revision: revision + 1, goal_sha256: 'updated-goal' });
      return true;
    };
    mocks.update
      .mockImplementationOnce(async () => {
        const error = 'goal update rejected for run run-a: run still has an active driver';
        useTaskRuntimeStore.setState({ error });
        throw new Error(error);
      })
      .mockImplementation(accepted);
    mocks.refresh.mockImplementation(async () => useTaskRuntimeStore.setState({ error: null }));
    render(<ChatPanel />);
    beginEdit();
    await waitFor(() => expect(screen.getByText(/目标已更新；/)).toBeTruthy());
    expect(mocks.update).toHaveBeenCalledTimes(2);
  });

  it.each(['false', 'throw'] as const)(
    'does not update after a %s pause result',
    async (failure) => {
      updateRun({ status: 'pending' });
      if (failure === 'false')
        mocks.pause.mockResolvedValue({ success: false, run_id: run.run_id });
      else mocks.pause.mockRejectedValue(new Error('pause observation failed'));
      render(<ChatPanel />);
      beginEdit('保留暂停失败草稿');
      await waitFor(() => expect(screen.getByRole('alert')).toBeTruthy());
      expect(mocks.update).not.toHaveBeenCalled();
      expect(
        (screen.getByRole('textbox', { name: '编辑当前目标' }) as HTMLInputElement).value
      ).toBe('保留暂停失败草稿');
    }
  );

  it('stops at a failed canonical read and preserves the draft', async () => {
    mocks.refresh.mockImplementation(async () =>
      useTaskRuntimeStore.setState({ error: 'journal unavailable' })
    );
    render(<ChatPanel />);
    beginEdit();
    await waitFor(() =>
      expect(screen.getByRole('alert').textContent).toContain('journal unavailable')
    );
    expect(mocks.update).not.toHaveBeenCalled();
    expect(screen.getByRole('textbox', { name: '编辑当前目标' })).toBeTruthy();
  });

  it('can retry a retained draft after a fresh read clears the previous backend error', async () => {
    mocks.update.mockImplementationOnce(async () => {
      useTaskRuntimeStore.setState({ error: 'CAS conflict' });
      throw new Error('CAS conflict');
    });
    mocks.refresh.mockImplementation(async () => useTaskRuntimeStore.setState({ error: null }));
    render(<ChatPanel />);
    beginEdit('失败后仍然保留');
    await waitFor(() => expect(screen.getByRole('alert').textContent).toContain('CAS conflict'));
    fireEvent.click(screen.getByRole('button', { name: /^保存$/ }));
    await waitFor(() => expect(screen.getByText(/目标已更新；/)).toBeTruthy());
    expect(mocks.update).toHaveBeenCalledTimes(2);
  });

  it('ends a bounded safe-point observation with the draft retained and no Goal mutation', async () => {
    vi.useFakeTimers();
    updateRun({ status: 'running' });
    mocks.pause.mockResolvedValue({ success: true, run_id: run.run_id });
    render(<ChatPanel />);
    beginEdit('等待超时仍保留');
    await act(async () => {
      await vi.advanceTimersByTimeAsync(30_000);
    });
    expect(screen.getByRole('alert').textContent).toContain('等待安全点超时');
    expect(mocks.update).not.toHaveBeenCalled();
    expect((screen.getByRole('textbox', { name: '编辑当前目标' }) as HTMLInputElement).value).toBe(
      '等待超时仍保留'
    );
    expect(liveRun()?.goal).toBe(run.goal);
  });

  it('keeps the captured edit revision when another update arrives while typing', async () => {
    render(<ChatPanel />);
    fireEvent.click(screen.getByRole('button', { name: '编辑当前目标' }));
    fireEvent.change(screen.getByRole('textbox', { name: '编辑当前目标' }), {
      target: { value: '旧版本草稿' },
    });
    act(() => updateRun({ goal: '外部更新', goal_revision: 2 }));
    fireEvent.click(screen.getByRole('button', { name: /^保存$/ }));
    await waitFor(() => expect(screen.getByRole('alert').textContent).toContain('目标版本已变化'));
    expect(mocks.update).not.toHaveBeenCalled();
    expect((screen.getByRole('textbox', { name: '编辑当前目标' }) as HTMLInputElement).value).toBe(
      '旧版本草稿'
    );
  });

  it.each(['false', 'throw', 'foreign'] as const)(
    'reports %s cancellation without success',
    async (failure) => {
      if (failure === 'throw') mocks.cancel.mockRejectedValue(new Error('cancel backend error'));
      else
        mocks.cancel.mockResolvedValue({
          success: failure === 'foreign',
          run_id: failure === 'foreign' ? 'other' : run.run_id,
        });
      render(<ChatPanel />);
      fireEvent.click(screen.getByRole('button', { name: '结束并删除当前目标' }));
      await waitFor(() => expect(screen.getByRole('alert')).toBeTruthy());
      expect(screen.queryByText('目标已结束')).toBeNull();
      expect(screen.queryByText('结束请求已接受，等待运行结算')).toBeNull();
    }
  );

  it('shows accepted cancellation as awaiting settlement while the canonical run is nonterminal', async () => {
    render(<ChatPanel />);
    fireEvent.click(screen.getByRole('button', { name: '结束并删除当前目标' }));
    await waitFor(() => expect(screen.getByText('结束请求已接受，等待运行结算')).toBeTruthy());
    expect(screen.queryByText('目标已结束')).toBeNull();
  });

  it('removes the Goal only after the canonical run becomes cancelled', async () => {
    mocks.cancel.mockImplementation(async () => {
      updateRun({ status: 'cancelled' });
      return { success: true, run_id: run.run_id };
    });
    render(<ChatPanel />);
    fireEvent.click(screen.getByRole('button', { name: '结束并删除当前目标' }));
    await waitFor(() => expect(screen.queryByRole('region', { name: '当前目标' })).toBeNull());
    expect(liveRun()?.goal).toBe(run.goal);
  });

  it.each(['conversation', 'workspace', 'generation', 'run', 'draft-epoch'] as const)(
    'fences a late pause when %s changes',
    async (change) => {
      updateRun({ status: 'running' });
      const pending = deferred<TaskRunControlReceipt>();
      mocks.pause.mockReturnValue(pending.promise);
      render(<ChatPanel />);
      beginEdit();
      act(() => {
        if (change === 'conversation')
          useConversationStore.setState({ activeId: 'conversation-b' });
        if (change === 'workspace')
          useWorkspaceStore.setState({ current: { ...workspace, id: 'workspace-b' } });
        if (change === 'generation')
          useWorkspaceStore.setState({
            current: { ...workspace, product_data_generation: 'generation-b' },
          });
        if (change === 'run') updateRun({ run_id: 'run-b' });
        if (change === 'draft-epoch') useConversationStore.setState({ newConversationEpoch: 2 });
      });
      await act(async () => pending.resolve({ success: true, run_id: run.run_id }));
      expect(mocks.refresh).not.toHaveBeenCalled();
      expect(mocks.update).not.toHaveBeenCalled();
      expect(screen.queryByText(/目标已更新；/)).toBeNull();
    }
  );

  it('does not resume an old operation after leaving and returning to the same run', async () => {
    updateRun({ status: 'running' });
    const pending = deferred<TaskRunControlReceipt>();
    mocks.pause.mockReturnValue(pending.promise);
    render(<ChatPanel />);
    beginEdit('旧稿');
    act(() => useConversationStore.setState({ activeId: 'other' }));
    act(() => useConversationStore.setState({ activeId: run.conversation_id }));
    fireEvent.click(screen.getByRole('button', { name: '编辑当前目标' }));
    fireEvent.change(screen.getByRole('textbox', { name: '编辑当前目标' }), {
      target: { value: '回到当前运行的新稿' },
    });
    await act(async () => pending.resolve({ success: true, run_id: run.run_id }));
    expect(mocks.update).not.toHaveBeenCalled();
    expect(mocks.refresh).not.toHaveBeenCalled();
    expect((screen.getByRole('textbox', { name: '编辑当前目标' }) as HTMLInputElement).value).toBe(
      '回到当前运行的新稿'
    );
  });

  it('keeps a successor run busy when an older same-view CAS receipt settles', async () => {
    const old = deferred<boolean>();
    const next = deferred<boolean>();
    mocks.update.mockReturnValueOnce(old.promise).mockReturnValueOnce(next.promise);
    render(<ChatPanel />);
    beginEdit('A draft');
    await waitFor(() => expect(mocks.update).toHaveBeenCalledTimes(1));
    act(() => updateRun({ run_id: 'run-b', goal: 'B goal' }));
    beginEdit('B draft');
    await waitFor(() => expect(mocks.update).toHaveBeenCalledTimes(2));
    await act(async () => old.resolve(true));
    expect((screen.getByRole('textbox', { name: '编辑当前目标' }) as HTMLInputElement).value).toBe(
      'B draft'
    );
    expect((screen.getByRole('button', { name: /^保存$/ }) as HTMLButtonElement).disabled).toBe(
      true
    );
    expect(screen.queryByText(/目标已更新；/)).toBeNull();
    await act(async () => next.resolve(false));
    expect((screen.getByRole('textbox', { name: '编辑当前目标' }) as HTMLInputElement).value).toBe(
      'B draft'
    );
  });
});

describe('composer Steer controls', () => {
  it('通过可聚焦按钮按相邻输入身份排序，首尾方向不发送请求', () => {
    mocks.queue = [queueItem('a'), queueItem('b'), queueItem('c')];
    render(<ChatPanel />);
    const first = within(screen.getByRole('group', { name: 'Steer 1' }));
    const middle = within(screen.getByRole('group', { name: 'Steer 2' }));
    const last = within(screen.getByRole('group', { name: 'Steer 3' }));
    expect((first.getByRole('button', { name: '上移 Steer' }) as HTMLButtonElement).disabled).toBe(
      true
    );
    expect((last.getByRole('button', { name: '下移 Steer' }) as HTMLButtonElement).disabled).toBe(
      true
    );
    const up = middle.getByRole('button', { name: '上移 Steer' });
    up.focus();
    expect(document.activeElement).toBe(up);
    fireEvent.click(up);
    expect(mocks.reorder).toHaveBeenLastCalledWith('b', 'a');
    fireEvent.click(middle.getByRole('button', { name: '下移 Steer' }));
    expect(mocks.reorder).toHaveBeenLastCalledWith('b', 'c');
    expect(mocks.reorder).toHaveBeenCalledTimes(2);
  });

  it('键盘排序不能跨相邻渠道输入或绕过正在处理的编辑', async () => {
    mocks.queue = [queueItem('a'), { ...queueItem('channel'), channelOwned: true }, queueItem('b')];
    const view = render(<ChatPanel />);
    expect(
      (
        within(screen.getByRole('group', { name: 'Steer 1' })).getByRole('button', {
          name: '下移 Steer',
        }) as HTMLButtonElement
      ).disabled
    ).toBe(true);
    expect(
      (
        within(screen.getByRole('group', { name: 'Steer 3' })).getByRole('button', {
          name: '上移 Steer',
        }) as HTMLButtonElement
      ).disabled
    ).toBe(true);
    mocks.queue = [queueItem('a'), queueItem('b')];
    view.rerender(<ChatPanel />);
    const pending = deferred<boolean>();
    mocks.replace.mockReturnValueOnce(pending.promise);
    const first = within(screen.getByRole('group', { name: 'Steer 1' }));
    fireEvent.click(first.getByRole('button', { name: '编辑排队消息' }));
    fireEvent.click(first.getByRole('button', { name: '保存 Steer 编辑' }));
    expect((first.getByRole('button', { name: '下移 Steer' }) as HTMLButtonElement).disabled).toBe(
      true
    );
    fireEvent.click(first.getByRole('button', { name: '下移 Steer' }));
    expect(mocks.reorder).not.toHaveBeenCalled();
    await act(async () => pending.resolve(false));
    expect((first.getByRole('button', { name: '下移 Steer' }) as HTMLButtonElement).disabled).toBe(
      false
    );
  });

  it.each(['composition', 'webkit'] as const)(
    'keeps a %s IME confirmation in the editor until a separate save',
    async (mode) => {
      mocks.queue = [queueItem('a')];
      render(<ChatPanel />);
      fireEvent.click(screen.getByRole('button', { name: '编辑排队消息' }));
      const editor = screen.getByRole('textbox', { name: '编辑排队消息' });
      fireEvent.change(editor, { target: { value: '尚在选词的中文' } });
      fireEvent.keyDown(editor, {
        key: 'Enter',
        isComposing: mode === 'composition',
        keyCode: mode === 'webkit' ? 229 : 13,
      });
      expect(mocks.replace).not.toHaveBeenCalled();
      expect((editor as HTMLInputElement).value).toBe('尚在选词的中文');
      fireEvent.keyDown(editor, { key: 'Enter', isComposing: false, keyCode: 13 });
      await waitFor(() =>
        expect(mocks.replace).toHaveBeenCalledExactlyOnceWith('a', '尚在选词的中文')
      );
    }
  );

  it('uses the original edit/reorder/delete/execute callbacks for the addressed queue item', async () => {
    mocks.queue = [queueItem('a'), queueItem('b')];
    render(<ChatPanel />);
    expect(screen.getByText('Steer · 2 条')).toBeTruthy();
    const a = screen.getByRole('group', { name: 'Steer 1' });
    const b = screen.getByRole('group', { name: 'Steer 2' });
    fireEvent.dragStart(a);
    fireEvent.dragOver(b);
    fireEvent.drop(b);
    expect(mocks.reorder).toHaveBeenCalledWith('a', 'b');
    fireEvent.click(within(a).getByRole('button', { name: '编辑排队消息' }));
    fireEvent.change(within(a).getByRole('textbox', { name: '编辑排队消息' }), {
      target: { value: '修改后的 Steer 🧭' },
    });
    fireEvent.click(within(a).getByRole('button', { name: '保存 Steer 编辑' }));
    await waitFor(() => expect(mocks.replace).toHaveBeenCalledWith('a', '修改后的 Steer 🧭'));
    await waitFor(() => expect(within(a).queryByRole('textbox')).toBeNull());
    fireEvent.click(within(b).getByRole('button', { name: '立即执行 Steer' }));
    await waitFor(() => expect(mocks.steer).toHaveBeenCalledWith('b'));
    await waitFor(() => expect(screen.getByText('已送达当前任务')).toBeTruthy());
    fireEvent.click(within(b).getByRole('button', { name: '移出队列' }));
    expect(mocks.remove).toHaveBeenCalledWith('b');
  });

  it.each(['false', 'throw'] as const)(
    'keeps the edit draft after a %s replacement',
    async (failure) => {
      mocks.queue = [queueItem('a')];
      if (failure === 'false') mocks.replace.mockResolvedValue(false);
      else mocks.replace.mockRejectedValue(new Error('queue revision changed'));
      render(<ChatPanel />);
      fireEvent.click(screen.getByRole('button', { name: '编辑排队消息' }));
      fireEvent.change(screen.getByRole('textbox', { name: '编辑排队消息' }), {
        target: { value: '不丢草稿' },
      });
      fireEvent.click(screen.getByRole('button', { name: '保存 Steer 编辑' }));
      await waitFor(() => expect(screen.getByRole('alert')).toBeTruthy());
      expect(
        (screen.getByRole('textbox', { name: '编辑排队消息' }) as HTMLInputElement).value
      ).toBe('不丢草稿');
    }
  );

  it('refuses to submit an editor whose underlying queue identity changed', async () => {
    mocks.queue = [queueItem('a')];
    const ui = render(<ChatPanel />);
    fireEvent.click(screen.getByRole('button', { name: '编辑排队消息' }));
    fireEvent.change(screen.getByRole('textbox', { name: '编辑排队消息' }), {
      target: { value: '过期草稿' },
    });
    mocks.queue = [
      {
        ...queueItem('a', '另一次编辑'),
        identity: { ...queueItem('a').identity, revision: 2, payload_sha256: 'new' },
      },
    ];
    ui.rerender(<ChatPanel />);
    fireEvent.click(screen.getByRole('button', { name: '保存 Steer 编辑' }));
    expect(mocks.replace).not.toHaveBeenCalled();
    expect(screen.getByRole('alert').textContent).toContain('排队消息已变化');
  });

  it('keeps an attachment-only edit without treating an unconfirmed execution as retained', async () => {
    mocks.queue = [
      {
        ...queueItem('a'),
        attachments: [{ name: 'a.txt', mime_type: 'text/plain', data: 'YQ==', size: 1 }],
      },
    ];
    mocks.steer.mockResolvedValue(false);
    render(<ChatPanel />);
    fireEvent.click(screen.getByRole('button', { name: '编辑排队消息' }));
    fireEvent.change(screen.getByRole('textbox', { name: '编辑排队消息' }), {
      target: { value: '' },
    });
    fireEvent.keyDown(screen.getByRole('textbox', { name: '编辑排队消息' }), { key: 'Enter' });
    await waitFor(() => expect(mocks.replace).toHaveBeenCalledWith('a', ''));
    await waitFor(() => expect(screen.queryByRole('textbox', { name: '编辑排队消息' })).toBeNull());
    fireEvent.click(screen.getByRole('button', { name: '立即执行 Steer' }));
    await waitFor(() =>
      expect(screen.getByText('尚未确认送达，请查看输入状态或错误后重试')).toBeTruthy()
    );
  });

  it('does not describe an explicit rejected Steer as safe-point waiting', async () => {
    mocks.queue = [{ ...queueItem('a'), steerOutcome: 'rejected' }];
    mocks.steer.mockResolvedValue(false);
    render(<ChatPanel />);
    fireEvent.click(screen.getByRole('button', { name: '立即执行 Steer' }));
    await waitFor(() =>
      expect(screen.getByText('尚未确认送达，请查看输入状态或错误后重试')).toBeTruthy()
    );
    expect(screen.getByText('已拒绝')).toBeTruthy();
    expect(screen.queryByText('已保留在队列，等待可用安全点')).toBeNull();
  });

  it('does not let a channel-owned input be dragged, edited, executed or removed', () => {
    mocks.queue = [{ ...queueItem('channel'), channelOwned: true }, queueItem('gui')];
    render(<ChatPanel />);
    const owned = screen.getByRole('group', { name: 'Steer 1' });
    const gui = screen.getByRole('group', { name: 'Steer 2' });
    expect(owned.draggable).toBe(false);
    expect(within(owned).getByText('渠道处理')).toBeTruthy();
    for (const button of within(owned).getAllByRole('button'))
      expect((button as HTMLButtonElement).disabled).toBe(true);
    fireEvent.dragStart(gui);
    fireEvent.drop(owned);
    fireEvent.dragStart(owned);
    fireEvent.drop(gui);
    expect(mocks.reorder).not.toHaveBeenCalled();
    expect(mocks.replace).not.toHaveBeenCalled();
    expect(mocks.remove).not.toHaveBeenCalled();
    expect(mocks.steer).not.toHaveBeenCalled();
  });
});
