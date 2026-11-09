// @vitest-environment jsdom
import { useState } from 'react';
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type {
  ConversationForkPreparation,
  ConversationForkReceipt,
  ProjectSource,
} from '../../api/endpoints';

const mocks = vi.hoisted(() => {
  Object.defineProperty(window, 'matchMedia', {
    configurable: true,
    value: () => ({ matches: false }),
  });
  return { prepare: vi.fn(), fork: vi.fn(), auth: vi.fn() };
});
vi.mock('../../api/endpoints', () => ({
  conversationApi: { prepareFork: mocks.prepare },
  sessionApi: {},
  toolExecutionApi: {},
}));
vi.mock('../workspace/ProjectSourceFields', async (original) => {
  const actual = await original<typeof import('../workspace/ProjectSourceFields')>();
  return {
    ...actual,
    ProjectSourceFields: (props: Parameters<typeof actual.ProjectSourceFields>[0]) => {
      mocks.auth(props);
      return (
        <button
          onClick={() =>
            props.onRemoteSource({
              kind: 'ssh',
              host: props.draft.host,
              user: props.draft.user,
              port: Number(props.draft.port),
              path: props.draft.remotePath,
              connection_id: props.draftConnectionId ?? '',
            } satisfies ProjectSource)
          }
        >
          测试同目标连接
        </button>
      );
    },
  };
});
import { ReplyForkDialog, type ReplyForkSource } from './ReplyForkDialog';
import { useConversationStore } from '../../stores/conversationStore';
import { useWorkspaceStore } from '../../stores/workspaceStore';
import { useToastStore } from '../../stores/toastStore';
import { useUiStore } from '../../stores/uiStore';

const originalFork = useConversationStore.getState().forkCurrent;
const source: ReplyForkSource = {
  scope: { workspaceId: 'global', workspaceGeneration: 'global' },
  conversationId: 'source-conversation',
  conversationEpoch: 3,
  navigationEpoch: 4,
  userTurnIndex: 2,
  requestId: 'stable-request-id',
};
const preparation: ConversationForkPreparation = {
  request_id: source.requestId,
  workspace_id: 'global',
  workspace_generation: 'global',
  source_id: source.conversationId,
  user_turn_index: 2,
  prefix_sha256: 'canonical-prefix',
  message_count: 8,
  worktree: {
    available: true,
    reason: null,
    repo_root: '/source',
    head: 'fixed-head-oid',
    branch: 'fork/new-branch',
    path: '/source-worktree',
    project_location: { kind: 'local', path: '/source' },
    ssh_target: null,
    source_connection_generation: null,
    draft_connection_id: null,
  },
};
const result = (): ConversationForkReceipt & { opened: boolean } => ({
  success: true,
  id: 'new-conversation',
  source_id: source.conversationId,
  source_workspace_id: 'global',
  request_id: source.requestId,
  destination: 'current_workspace',
  workspace_id: 'global',
  workspace_generation: 'global',
  message_count: 8,
  target_content: '',
  conversation_created: true,
  imported: true,
  opened: true,
});
function deferred<T>() {
  let resolve: (value: T) => void = () => undefined;
  const promise = new Promise<T>((next) => {
    resolve = next;
  });
  return { promise, resolve };
}
beforeEach(() => {
  vi.resetAllMocks();
  useWorkspaceStore.setState({ current: null, isLoading: false });
  useConversationStore.setState({
    activeId: source.conversationId,
    workspaceId: 'global',
    newConversationEpoch: 3,
    isLoading: false,
    forkCurrent: mocks.fork,
  });
  useUiStore.setState({ navigationEpoch: 4 });
  useToastStore.getState().clearAll();
  mocks.prepare.mockResolvedValue(structuredClone(preparation));
  mocks.fork.mockResolvedValue(result());
});
afterEach(() => {
  cleanup();
  useConversationStore.setState({ forkCurrent: originalFork });
});

describe('回复分叉目的地', () => {
  it('branch-only failure does not promise an entry in existing lists', async () => {
    mocks.fork.mockResolvedValueOnce({
      ...result(),
      success: false,
      opened: false,
      conversation_created: false,
      imported: false,
      id: null,
      workspace: null,
      worktree: {
        created: false,
        observed: false,
        path: '/source-worktree',
        head: 'fixed-head-oid',
      },
      error: 'Operation not permitted',
    });
    render(<ReplyForkDialog source={source} onClose={vi.fn()} />);
    await waitFor(() =>
      expect(screen.getByRole('button', { name: '创建分叉' }).hasAttribute('disabled')).toBe(false)
    );
    fireEvent.click(screen.getByRole('button', { name: '创建分叉' }));
    await waitFor(() =>
      expect(screen.getByRole('status').textContent).toContain('当前没有可打开的新项目')
    );
    expect(screen.getByRole('status').textContent).not.toContain('可从现有列表');
    expect(screen.getByRole('alert').textContent).toContain('Operation not permitted');
    expect(screen.getByRole('button', { name: '核对并重试' })).toBeTruthy();
  });

  it('confirmed worktree keeps its actual list recovery guidance after later failure', async () => {
    mocks.fork.mockResolvedValueOnce({
      ...result(),
      success: false,
      opened: false,
      conversation_created: false,
      imported: false,
      id: null,
      workspace: null,
      worktree: {
        created: true,
        observed: true,
        path: '/source-worktree',
        head: 'fixed-head-oid',
      },
      error: 'workspace creation failed',
    });
    render(<ReplyForkDialog source={source} onClose={vi.fn()} />);
    await waitFor(() =>
      expect(screen.getByRole('button', { name: '创建分叉' }).hasAttribute('disabled')).toBe(false)
    );
    fireEvent.click(screen.getByRole('button', { name: '创建分叉' }));
    await waitFor(() => expect(screen.getByRole('status').textContent).toMatch(/可从.*列表/));
    expect(screen.getByRole('alert').textContent).toContain('workspace creation failed');
  });

  it('说明两目的地，提交固定前缀、源身份与当前工作空间', async () => {
    const close = vi.fn();
    render(<ReplyForkDialog source={source} onClose={close} />);
    await waitFor(() =>
      expect(screen.getByRole('button', { name: '创建分叉' }).hasAttribute('disabled')).toBe(false)
    );
    expect(screen.getByText('从上次提交创建，未提交改动留原处。')).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: '创建分叉' }));
    await waitFor(() => expect(mocks.fork).toHaveBeenCalledTimes(1));
    expect(mocks.prepare).toHaveBeenCalledWith(
      source.scope,
      source.conversationId,
      2,
      source.requestId
    );
    expect(mocks.fork.mock.calls.at(0)?.at(0)).toEqual({
      preparation,
      destination: 'current_workspace',
    });
    await waitFor(() => expect(close).toHaveBeenCalledTimes(1));
  });

  it('同一提交双击只创建一次，重试保留目的地和完整请求', async () => {
    const first = deferred<ReturnType<typeof result>>();
    mocks.fork.mockReturnValueOnce(first.promise);
    render(<ReplyForkDialog source={source} onClose={vi.fn()} />);
    await waitFor(() =>
      expect(screen.getByRole('radio', { name: /新 Worktree/ }).hasAttribute('disabled')).toBe(
        false
      )
    );
    fireEvent.click(screen.getByRole('radio', { name: /新 Worktree/ }));
    const create = screen.getByRole('button', { name: '创建分叉' });
    fireEvent.click(create);
    fireEvent.click(create);
    expect(mocks.fork).toHaveBeenCalledTimes(1);
    await act(async () =>
      first.resolve({
        ...result(),
        success: false,
        opened: false,
        imported: false,
        id: null,
        destination: 'new_worktree',
        error: '导入失败',
        worktree: {
          path: '/source-worktree',
          head: 'fixed-head-oid',
          created: true,
          observed: true,
          descriptor: null,
          error: null,
        },
      })
    );
    expect(screen.getByRole('alert').textContent).toContain('导入失败');
    expect(screen.getByText(/资源状态：已创建 Worktree：\/source-worktree/)).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: '核对并重试' }));
    await waitFor(() => expect(mocks.fork).toHaveBeenCalledTimes(2));
    expect(mocks.fork.mock.calls.at(1)?.at(0)).toBe(mocks.fork.mock.calls.at(0)?.at(0));
  });

  it('非Git只禁用新Worktree并显示具体原因，当前工作空间仍可用', async () => {
    mocks.prepare.mockResolvedValueOnce({
      ...preparation,
      worktree: {
        ...preparation.worktree,
        available: false,
        reason: '该目录不是 Git 仓库。',
      },
    });
    render(<ReplyForkDialog source={source} onClose={vi.fn()} />);
    expect(await screen.findByText('该目录不是 Git 仓库。')).toBeTruthy();
    expect(screen.getByRole('radio', { name: /新 Worktree/ }).hasAttribute('disabled')).toBe(true);
    expect(screen.getByRole('radio', { name: /当前工作空间/ }).hasAttribute('disabled')).toBe(
      false
    );
    fireEvent.click(screen.getByRole('button', { name: '创建分叉' }));
    await waitFor(() => expect(mocks.fork).toHaveBeenCalledTimes(1));
  });

  it('来源切换后禁用创建；旧prepare迟到不会把新选择当来源', async () => {
    const delayed = deferred<ConversationForkPreparation>();
    mocks.prepare.mockReturnValueOnce(delayed.promise);
    render(<ReplyForkDialog source={source} onClose={vi.fn()} />);
    act(() => useConversationStore.setState({ activeId: 'another-conversation' }));
    await act(async () => delayed.resolve(preparation));
    expect(screen.getByRole('button', { name: '创建分叉' }).hasAttribute('disabled')).toBe(true);
    expect(screen.getByText(/来源已切换/)).toBeTruthy();
    expect(mocks.fork).not.toHaveBeenCalled();
  });

  it('关闭待完成操作取消观察资格，保留迟到的真实资源提示', async () => {
    const pending = deferred<ReturnType<typeof result>>();
    mocks.fork.mockReturnValueOnce(pending.promise);
    function Host() {
      const [open, setOpen] = useState(true);
      return open ? (
        <ReplyForkDialog source={source} onClose={() => setOpen(false)} />
      ) : (
        <p>已关闭</p>
      );
    }
    render(<Host />);
    await waitFor(() =>
      expect(screen.getByRole('button', { name: '创建分叉' }).hasAttribute('disabled')).toBe(false)
    );
    fireEvent.click(screen.getByRole('button', { name: '创建分叉' }));
    const canOpen = mocks.fork.mock.calls.at(0)?.at(1) as () => boolean;
    expect(canOpen()).toBe(true);
    fireEvent.click(screen.getByRole('button', { name: '关闭分叉' }));
    expect(canOpen()).toBe(false);
    await act(async () => pending.resolve({ ...result(), opened: false }));
    expect(screen.getByText('已关闭')).toBeTruthy();
    expect(useToastStore.getState().toasts.at(-1)?.message).toContain('最近任务');
  });

  it('SSH只接受prepare指定的独立draft并在提交后交给原命令持有', async () => {
    const remote = {
      ...preparation,
      worktree: {
        ...preparation.worktree!,
        ssh_target: {
          kind: 'ssh' as const,
          host: 'real.remote',
          user: 'echo',
          port: 22,
        },
        draft_connection_id: 'independent-target-draft',
        path: '/remote/worktree',
      },
    };
    mocks.prepare.mockResolvedValueOnce(remote);
    const pending = deferred<ReturnType<typeof result>>();
    mocks.fork.mockReturnValueOnce(pending.promise);
    render(<ReplyForkDialog source={source} onClose={vi.fn()} />);
    await waitFor(() =>
      expect(screen.getByRole('radio', { name: /新 Worktree/ }).hasAttribute('disabled')).toBe(
        false
      )
    );
    fireEvent.click(screen.getByRole('radio', { name: /新 Worktree/ }));
    fireEvent.click(screen.getByRole('button', { name: '测试同目标连接' }));
    fireEvent.click(screen.getByRole('button', { name: '创建分叉' }));
    expect(mocks.fork.mock.calls.at(0)?.at(0)).toMatchObject({
      preparation: remote,
      destination: 'new_worktree',
      ssh_connection_id: 'independent-target-draft',
    });
    expect(mocks.auth.mock.calls.at(-1)?.at(0)).toMatchObject({
      fixedLocation: true,
      draftConnectionId: 'independent-target-draft',
      retainConnection: true,
      draft: {
        host: 'real.remote',
        user: 'echo',
        port: '22',
        remotePath: '/remote/worktree',
      },
    });
    await act(async () => pending.resolve({ ...result(), destination: 'new_worktree' }));
  });

  it('partial resource receipts keep unverified copies visible without claiming imported history', async () => {
    mocks.fork.mockResolvedValueOnce({
      ...result(),
      success: false,
      opened: false,
      imported: false,
      resources: [
        {
          source_path: '/source/1',
          target_path: '/target/1',
          bytes: 6,
          sha256: 'a'.repeat(64),
          reader_verified: true,
        },
        {
          source_path: '/source/2',
          target_path: '/target/2',
          bytes: 9,
          sha256: 'b'.repeat(64),
          reader_verified: false,
        },
      ],
      error: '部分原文尚不能读取',
    });
    render(<ReplyForkDialog source={source} onClose={vi.fn()} />);
    await waitFor(() =>
      expect(screen.getByRole('button', { name: '创建分叉' }).hasAttribute('disabled')).toBe(false)
    );
    fireEvent.click(screen.getByRole('button', { name: '创建分叉' }));
    await waitFor(() =>
      expect(screen.getByText(/已保存 2 份历史资源，其中 1 份仍待核对/)).toBeTruthy()
    );
    expect(screen.getByRole('alert').textContent).toContain('部分原文尚不能读取');
    expect(screen.queryByText(/会话历史已复制/)).toBeNull();
  });

  it('shows a historical resource notice after opening without treating it as failed creation', async () => {
    mocks.fork.mockResolvedValueOnce({
      ...result(),
      warnings: ['历史文字未记录资源关联，已保留原文。'],
    });
    const onClose = vi.fn();
    render(<ReplyForkDialog source={source} onClose={onClose} />);
    await waitFor(() =>
      expect(screen.getByRole('button', { name: '创建分叉' }).hasAttribute('disabled')).toBe(false)
    );
    fireEvent.click(screen.getByRole('button', { name: '创建分叉' }));
    await waitFor(() => expect(onClose).toHaveBeenCalledTimes(1));
    expect(
      useToastStore
        .getState()
        .toasts.some(
          (toast) =>
            toast.type === 'warning' && toast.message === '历史文字未记录资源关联，已保留原文。'
        )
    ).toBe(true);
  });

  it('沿原Modal支持Escape并恢复入口焦点', async () => {
    function Host() {
      const [open, setOpen] = useState(false);
      return (
        <>
          <button onClick={() => setOpen(true)}>回复处分叉</button>
          {open && <ReplyForkDialog source={source} onClose={() => setOpen(false)} />}
        </>
      );
    }
    render(<Host />);
    const trigger = screen.getByRole('button', { name: '回复处分叉' });
    trigger.focus();
    fireEvent.click(trigger);
    expect(screen.getByRole('dialog', { name: '分叉会话' })).toBeTruthy();
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(screen.queryByRole('dialog')).toBeNull();
    expect(document.activeElement).toBe(trigger);
  });
});
