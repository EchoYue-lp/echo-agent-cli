// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { PendingHitlRequest } from '../../stores/chatStore';
import type { HitlRequestIdentity } from '../../types/api';
import { HitlDialog } from './HitlDialog';

afterEach(cleanup);

function requestIdentity(
  kind: HitlRequestIdentity['kind'],
  requestId: string
): HitlRequestIdentity {
  return {
    request_id: requestId,
    response_target: `original-target-${requestId}`,
    kind,
    scope: {
      workspace_id: 'workspace-original',
      host_generation: 'generation-original',
      conversation_id: 'conversation-original',
      root_turn_id: 'root-original',
      turn_id: 'turn-original',
      run_id: 'run-original',
      task_id: 'task-original',
      channel_id: 'feishu',
      channel_conversation_id: 'im-conversation-original',
      sender_id: 'sender-original',
      sender_incarnation: 'sender-generation-original',
    },
  };
}

const approval: PendingHitlRequest = {
  kind: 'approval',
  requestId: 'approval-original',
  identity: requestIdentity('approval', 'approval-original'),
  toolName: 'shell',
  args: { command: 'git status' },
  prompt: '查看本次改动，可以吗？',
};
const input: PendingHitlRequest = {
  kind: 'input',
  requestId: 'input-original',
  identity: requestIdentity('input', 'input-original'),
  prompt: '请补充你的偏好',
};
const selection: PendingHitlRequest = {
  kind: 'selection',
  requestId: 'selection-original',
  identity: requestIdentity('selection', 'selection-original'),
  prompt: '接下来如何处理？',
  options: ['保留现有改动', '仅修改指定文件'],
  taskId: 'task-original',
  phase: '方案确认',
};

function callbacks() {
  return {
    onApproval: vi.fn().mockResolvedValue(true),
    onInput: vi.fn().mockResolvedValue(true),
    onSelection: vi.fn().mockResolvedValue(true),
  };
}

describe('HitlDialog', () => {
  it('keeps reverse Tab on enabled details while a selection reply disables its fieldset', async () => {
    const props = callbacks();
    let finish: ((accepted: boolean) => void) | undefined;
    props.onSelection.mockImplementationOnce(
      () =>
        new Promise<boolean>((resolve) => {
          finish = resolve;
        })
    );
    render(<HitlDialog requests={[selection]} {...props} />);
    fireEvent.click(screen.getByRole('radio', { name: /保留现有改动/ }));
    fireEvent.click(screen.getByRole('button', { name: '提交回答' }));
    await waitFor(() => expect(props.onSelection).toHaveBeenCalledOnce());
    const close = screen
      .getAllByRole('button', { name: '稍后处理' })
      .find((button) => !(button as HTMLButtonElement).disabled);
    const details = screen.getByText('请求详情').closest('summary');
    if (!close || !details) throw new Error('HITL modal controls missing');
    close.focus();
    fireEvent.keyDown(document, { key: 'Tab', shiftKey: true });
    expect(document.activeElement).toBe(details);
    await act(async () => finish?.(true));
  });

  it('unifies the FIFO queue and hides without approving, cancelling or losing discovery', () => {
    const props = callbacks();
    render(
      <HitlDialog
        requests={[approval, input, selection]}
        {...props}
        conversationLabel="整理工作区"
      />
    );
    expect(screen.getByRole('dialog', { name: '操作确认' })).toBeTruthy();
    expect(screen.getByText('整理工作区')).toBeTruthy();
    fireEvent.click(screen.getByText('另有 2 项等待处理'));
    expect(screen.getByText('请补充你的偏好')).toBeTruthy();
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(screen.queryByRole('dialog')).toBeNull();
    expect(props.onApproval).not.toHaveBeenCalled();
    expect(props.onInput).not.toHaveBeenCalled();
    expect(props.onSelection).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole('button', { name: /Eclip 正在等你回复/ }));
    expect(screen.getByRole('dialog', { name: '操作确认' })).toBeTruthy();
  });

  it('preserves a multiline Unicode draft across close and re-open, forwarding the original request', async () => {
    const props = callbacks();
    render(<HitlDialog requests={[input]} {...props} />);
    const answer = screen.getByRole('textbox', { name: '你的回答' });
    expect(document.activeElement).toBe(answer);
    fireEvent.change(answer, { target: { value: '保留中文🙂\n还有第二行 ' } });
    fireEvent.click(screen.getByRole('button', { name: '关闭对话框' }));
    fireEvent.click(screen.getByRole('button', { name: /Eclip 正在等你回复/ }));
    expect((screen.getByRole('textbox') as HTMLTextAreaElement).value).toBe(
      '保留中文🙂\n还有第二行 '
    );
    fireEvent.click(screen.getByRole('button', { name: '提交回答' }));
    await waitFor(() =>
      expect(props.onInput).toHaveBeenCalledWith(input, '保留中文🙂\n还有第二行 ')
    );
    expect(screen.getByRole('status').textContent).toContain('等待请求方确认');
  });

  it('keeps the failed answer for retry and does not call a failed submission a winner', async () => {
    const props = callbacks();
    props.onInput.mockRejectedValueOnce(new Error('请求暂不可用')).mockResolvedValueOnce(true);
    render(<HitlDialog requests={[input]} {...props} />);
    fireEvent.change(screen.getByRole('textbox'), { target: { value: '不要丢失这段回答' } });
    fireEvent.click(screen.getByRole('button', { name: '提交回答' }));
    await waitFor(() => expect(screen.getByRole('alert').textContent).toContain('请求暂不可用'));
    expect((screen.getByRole('textbox') as HTMLTextAreaElement).value).toBe('不要丢失这段回答');
    expect(screen.queryByRole('status')).toBeNull();
    fireEvent.click(screen.getByRole('button', { name: '提交回答' }));
    await waitFor(() => expect(props.onInput).toHaveBeenCalledTimes(2));
  });

  it('treats a false transport acknowledgement as a retryable failure', async () => {
    const props = callbacks();
    props.onInput.mockResolvedValue(false);
    render(<HitlDialog requests={[input]} {...props} />);
    fireEvent.change(screen.getByRole('textbox'), { target: { value: '需要保留' } });
    fireEvent.click(screen.getByRole('button', { name: '提交回答' }));
    await waitFor(() => expect(screen.getByRole('alert')).toBeTruthy());
    expect(screen.queryByRole('status')).toBeNull();
  });

  it('submits only the selected literal option and preserves original request identity', async () => {
    const props = callbacks();
    render(<HitlDialog requests={[selection]} {...props} />);
    const submit = screen.getByRole('button', { name: '提交回答' }) as HTMLButtonElement;
    expect(submit.disabled).toBe(true);
    fireEvent.click(screen.getByRole('radio', { name: /仅修改指定文件/ }));
    fireEvent.change(screen.getByRole('textbox'), { target: { value: ' 不要清理其它内容🙂 ' } });
    fireEvent.click(submit);
    await waitFor(() =>
      expect(props.onSelection).toHaveBeenCalledWith(
        selection,
        '仅修改指定文件',
        '不要清理其它内容🙂'
      )
    );
  });

  it('retains the existing empty-option continue response', async () => {
    const request: PendingHitlRequest = { ...selection, options: [] };
    const props = callbacks();
    render(<HitlDialog requests={[request]} {...props} />);
    fireEvent.click(screen.getByRole('button', { name: '提交回答' }));
    await waitFor(() =>
      expect(props.onSelection).toHaveBeenCalledWith(request, 'approve', undefined)
    );
  });

  it('distinguishes rejection, one-time approval and the existing session-tool scope', async () => {
    const props = callbacks();
    const { unmount } = render(<HitlDialog requests={[approval]} {...props} />);
    fireEvent.click(screen.getByRole('button', { name: '拒绝' }));
    fireEvent.change(screen.getByRole('textbox'), { target: { value: '先确认路径' } });
    fireEvent.click(screen.getByRole('button', { name: '确认拒绝' }));
    await waitFor(() =>
      expect(props.onApproval).toHaveBeenCalledWith(approval, false, '先确认路径')
    );
    unmount();
    const next = render(<HitlDialog requests={[approval]} {...props} />);
    fireEvent.click(screen.getByRole('button', { name: '允许这次' }));
    await waitFor(() => expect(props.onApproval).toHaveBeenCalledWith(approval, true));
    next.unmount();
    render(<HitlDialog requests={[approval]} {...props} />);
    fireEvent.click(screen.getByRole('button', { name: '本会话允许此工具' }));
    await waitFor(() =>
      expect(props.onApproval).toHaveBeenCalledWith(approval, true, undefined, 'session_tool')
    );
  });

  it('blocks same-frame duplicates while a response is in flight', async () => {
    const props = callbacks();
    let finish: ((value: boolean) => void) | undefined;
    props.onApproval.mockImplementation(
      () =>
        new Promise<boolean>((resolve) => {
          finish = resolve;
        })
    );
    render(<HitlDialog requests={[approval]} {...props} />);
    const button = screen.getByRole('button', { name: '允许这次' });
    fireEvent.click(button);
    fireEvent.click(button);
    expect(props.onApproval).toHaveBeenCalledOnce();
    await act(async () => {
      finish?.(true);
    });
  });

  it('does not submit an IME composition through the keyboard shortcut', async () => {
    const props = callbacks();
    render(<HitlDialog requests={[input]} {...props} />);
    const answer = screen.getByRole('textbox');
    fireEvent.change(answer, { target: { value: '正在输入中文' } });
    fireEvent.compositionStart(answer);
    fireEvent.keyDown(answer, { key: 'Enter', ctrlKey: true, isComposing: true });
    expect(props.onInput).not.toHaveBeenCalled();
    fireEvent.compositionEnd(answer);
    fireEvent.keyDown(answer, { key: 'Enter', ctrlKey: true });
    await waitFor(() => expect(props.onInput).toHaveBeenCalledOnce());
  });

  it('keeps a reused request id separated by the original delivered identity', async () => {
    const props = callbacks();
    let finishOld: ((value: boolean) => void) | undefined;
    props.onInput.mockImplementationOnce(
      () =>
        new Promise<boolean>((resolve) => {
          finishOld = resolve;
        })
    );
    const successor: PendingHitlRequest = {
      ...input,
      identity: {
        ...input.identity,
        response_target: 'successor-target',
        scope: { ...input.identity.scope, sender_incarnation: 'successor-generation' },
      },
    };
    const { rerender } = render(<HitlDialog requests={[input]} {...props} />);
    fireEvent.change(screen.getByRole('textbox'), { target: { value: '旧请求草稿' } });
    fireEvent.click(screen.getByRole('button', { name: '提交回答' }));
    rerender(<HitlDialog requests={[successor]} {...props} />);
    await act(async () => {
      finishOld?.(true);
    });
    expect((screen.getByRole('textbox') as HTMLTextAreaElement).value).toBe('');
    expect(screen.queryByRole('status')).toBeNull();
    fireEvent.change(screen.getByRole('textbox'), { target: { value: '新请求回答' } });
    fireEvent.click(screen.getByRole('button', { name: '提交回答' }));
    await waitFor(() => expect(props.onInput).toHaveBeenCalledTimes(2));
    const delivered = props.onInput.mock.calls.at(-1)?.at(0);
    expect(delivered).toBe(successor);
    expect(delivered.identity).toBe(successor.identity);
  });

  it('discards an old presentation after expiry and ignores its late acknowledgement', async () => {
    const props = callbacks();
    let finish: ((value: boolean) => void) | undefined;
    props.onInput.mockImplementation(
      () =>
        new Promise<boolean>((resolve) => {
          finish = resolve;
        })
    );
    const { rerender } = render(<HitlDialog requests={[input]} {...props} />);
    fireEvent.change(screen.getByRole('textbox'), { target: { value: '旧回复' } });
    fireEvent.click(screen.getByRole('button', { name: '提交回答' }));
    rerender(<HitlDialog requests={[selection]} {...props} />);
    await act(async () => {
      finish?.(true);
    });
    expect(screen.getByRole('dialog', { name: '选择下一步' })).toBeTruthy();
    expect(screen.queryByRole('status')).toBeNull();
    expect((screen.getByRole('textbox') as HTMLTextAreaElement).value).toBe('');
    rerender(<HitlDialog requests={[]} {...props} />);
    expect(screen.queryByRole('dialog')).toBeNull();
    expect(screen.queryByRole('button')).toBeNull();
  });
});
