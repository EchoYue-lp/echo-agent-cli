// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { ChatSendSubmission } from '../../hooks/useTauriChat';
import type { Attachment } from '../../types/api';
import type { ConversationInputReceipt } from '../../generated';
import type { Workspace } from '../../api/endpoints';

const mocks = vi.hoisted(() => {
  Object.defineProperty(window, 'matchMedia', {
    configurable: true,
    value: () => ({ matches: false }),
  });
  return { models: vi.fn(), addToast: vi.fn() };
});
vi.mock('../../api/endpoints', async (original) => {
  const actual = await original<typeof import('../../api/endpoints')>();
  return {
    ...actual,
    providerApi: {
      ...actual.providerApi,
      listConfigured: mocks.models,
      setThinking: vi.fn().mockResolvedValue({ success: true, applied: true, spec: 'auto' }),
    },
    permissionsApi: {
      ...actual.permissionsApi,
      getMode: vi.fn().mockResolvedValue({ mode: 'default' }),
    },
  };
});

import { ChatInput } from './ChatInput';
import { useWorkspaceStore } from '../../stores/workspaceStore';
import { useConversationStore } from '../../stores/conversationStore';
import { useToastStore } from '../../stores/toastStore';

function deferred<T>() {
  let resolve: (value: T) => void = () => undefined;
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

// Hold the real browser boundary, rather than replacing the editor's admission
// code. Keyboard events can race while its FileReader promise is unresolved.
class HeldFileReader {
  static reads: HeldFileReader[] = [];
  result: string | ArrayBuffer | null = null;
  onload: ((event: ProgressEvent) => void) | null = null;
  onerror: ((event: ProgressEvent) => void) | null = null;
  onabort: ((event: ProgressEvent) => void) | null = null;
  readAsDataURL() {
    HeldFileReader.reads.push(this);
  }
  complete(result = 'data:text/plain;base64,aGVsbG8=') {
    this.result = result;
    this.onload?.(new ProgressEvent('load'));
  }
  fail() {
    this.onerror?.(new ProgressEvent('error'));
  }
  abort() {
    this.onabort?.(new ProgressEvent('abort'));
  }
}

const workspace: Workspace = {
  id: 'workspace-a',
  name: 'Workspace A',
  root: '/data/a',
  project_root: '/code/a',
  kind: { type: 'code' },
  metadata: { tags: [], project_location: { kind: 'local' as const, path: '/code/a' } },
  product_data_generation: 'a-v1',
  created_at: '',
  last_active: '',
};
type Send = (
  text: string,
  attachments?: Attachment[],
  submission?: ChatSendSubmission
) => Promise<boolean>;
const originalAddToast = useToastStore.getState().addToast;

beforeEach(() => {
  vi.clearAllMocks();
  HeldFileReader.reads = [];
  vi.stubGlobal('FileReader', HeldFileReader);
  vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) => {
    callback(0);
    return 1;
  });
  vi.stubGlobal('cancelAnimationFrame', vi.fn());
  Object.defineProperty(URL, 'createObjectURL', {
    configurable: true,
    value: vi.fn(() => 'blob:owned-preview'),
  });
  Object.defineProperty(URL, 'revokeObjectURL', { configurable: true, value: vi.fn() });
  Object.defineProperty(HTMLElement.prototype, 'scrollIntoView', {
    configurable: true,
    value: vi.fn(),
  });
  localStorage.clear();
  useWorkspaceStore.setState({ current: workspace, workspaces: [workspace], isLoading: false });
  useConversationStore.setState({
    workspaceId: workspace.id,
    activeId: 'conversation-a',
    newConversationEpoch: 0,
    isLoading: false,
  });
  useToastStore.setState({ toasts: [], addToast: mocks.addToast });
  mocks.models.mockResolvedValue({
    default_model_id: 'provider:model',
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
  });
});
afterEach(() => {
  cleanup();
  useToastStore.setState({ toasts: [], addToast: originalAddToast });
  vi.unstubAllGlobals();
});

async function editor(onSend = vi.fn<Send>().mockResolvedValue(false)) {
  const view = render(<ChatInput onSend={onSend} />);
  await waitFor(() =>
    expect(view.getByRole('button', { name: '选择模型' }).textContent).toContain('Model')
  );
  const input = view.getByRole('textbox', { name: '消息' }) as HTMLTextAreaElement;
  const type = (text: string) => fireEvent.change(input, { target: { value: text } });
  const send = () => fireEvent.keyDown(input, { key: 'Enter' });
  const attach = (name = 'note.txt', type = 'text/plain') => {
    const fileInput = view.container.querySelector<HTMLInputElement>('input[type="file"]');
    if (!fileInput) throw new Error('Missing real attachment input');
    fireEvent.change(fileInput, { target: { files: [new File(['hello'], name, { type })] } });
  };
  return { ...view, input, type, send, attach, onSend };
}

function submissionOf(onSend: ReturnType<typeof vi.fn<Send>>, index = 0): ChatSendSubmission {
  const submission = onSend.mock.calls[index]?.[2];
  if (!submission) throw new Error('The editor did not provide its submission descriptor');
  return submission;
}
function persisted(submission: ChatSendSubmission): ConversationInputReceipt {
  return {
    identity: {
      address: {
        workspace_id: submission.workspaceId,
        conversation_id: submission.conversationId ?? 'canonical-first',
      },
      input_id: submission.externalId,
      revision: 1,
      payload_sha256: 'original-payload-hash',
    },
    phase: 'persisted',
    attempt: null,
    attempt_id: null,
    turn_id: null,
    outcome: null,
    drained: false,
    reason: null,
    duplicate: false,
    queue_revision: 1,
  };
}
async function acknowledge(submission: ChatSendSubmission, canProject = () => true) {
  await act(async () => {
    await submission.onPersisted?.(persisted(submission), canProject);
  });
}

describe('ChatInput original draft admission', () => {
  it('admits one identity while repeated Enter/click races the held attachment encoder', async () => {
    const response = deferred<boolean>();
    const onSend = vi.fn<Send>().mockReturnValue(response.promise);
    const view = await editor(onSend);
    view.type('原始输入');
    view.attach();
    view.send();
    view.send();
    fireEvent.click(view.getByRole('button', { name: '发送' }));
    expect(HeldFileReader.reads).toHaveLength(1);
    expect(onSend).not.toHaveBeenCalled();
    expect(view.input.getAttribute('aria-busy')).toBe('true');
    await act(async () => HeldFileReader.reads[0]?.complete());
    expect(onSend).toHaveBeenCalledTimes(1);
    const submission = submissionOf(onSend);
    expect(submission).toMatchObject({
      workspaceId: workspace.id,
      workspaceGeneration: 'a-v1',
      conversationId: 'conversation-a',
      newConversationEpoch: 0,
    });
    expect(submission.externalId).toBeTruthy();
    expect(submission.payload).toEqual({
      text: '原始输入',
      attachments: [
        { name: 'note.txt', mime_type: 'text/plain', data: 'aGVsbG8=', size: 5, source: 'upload' },
      ],
    });
    await act(async () => response.resolve(false));
    expect(view.input.value).toBe('原始输入');
    expect(view.getByText('note.txt')).toBeTruthy();
  });

  it('clears a persisted draft before the held drain completes and protects the next draft from the old boolean', async () => {
    const response = deferred<boolean>();
    const onSend = vi.fn<Send>().mockReturnValue(response.promise);
    const view = await editor(onSend);
    view.type('持久输入');
    view.attach('image.png', 'image/png');
    view.send();
    await act(async () => HeldFileReader.reads[0]?.complete('data:image/png;base64,aGVsbG8='));
    const original = submissionOf(onSend);
    await acknowledge(original);
    expect(view.input.value).toBe('');
    expect(view.queryByText('image.png')).toBeNull();
    expect(URL.revokeObjectURL).toHaveBeenCalledWith('blob:owned-preview');
    expect(view.getByText('已收到')).toBeTruthy();
    expect(view.input.getAttribute('aria-busy')).toBe('false');
    view.type('下一条草稿');
    await act(async () => response.resolve(true));
    expect(view.input.value).toBe('下一条草稿');
    expect(view.queryByText('已收到')).toBeNull();
    view.send();
    await waitFor(() => expect(onSend).toHaveBeenCalledTimes(2));
    expect(submissionOf(onSend, 1).externalId).not.toBe(original.externalId);
  });

  it('preserves edits made before the receipt, but releases the editor for the next explicit send immediately', async () => {
    const first = deferred<boolean>();
    const second = deferred<boolean>();
    const onSend = vi
      .fn<Send>()
      .mockReturnValueOnce(first.promise)
      .mockReturnValueOnce(second.promise);
    const view = await editor(onSend);
    view.type('先发这条');
    view.send();
    await waitFor(() => expect(onSend).toHaveBeenCalledTimes(1));
    const original = submissionOf(onSend);
    view.type('正在编辑的新稿');
    await acknowledge(original);
    expect(view.input.value).toBe('正在编辑的新稿');
    expect(view.queryByText('已收到')).toBeNull();
    expect(view.input.getAttribute('aria-busy')).toBe('false');
    view.send();
    await waitFor(() => expect(onSend).toHaveBeenCalledTimes(2));
    const next = submissionOf(onSend, 1);
    expect(next.externalId).not.toBe(original.externalId);
    await act(async () => first.resolve(true));
    expect(view.input.value).toBe('正在编辑的新稿');
    expect(view.input.getAttribute('aria-busy')).toBe('true');
    await acknowledge(next);
    expect(view.input.value).toBe('');
    await act(async () => second.resolve(true));
  });

  it('reuses the same input identity and exact encoded payload after a lost acknowledgement', async () => {
    const onSend = vi
      .fn<Send>()
      .mockRejectedValueOnce(new Error('ack lost'))
      .mockResolvedValueOnce(false);
    const view = await editor(onSend);
    view.type('  不能重复持久化  ');
    view.attach();
    view.send();
    await act(async () => HeldFileReader.reads[0]?.complete());
    await waitFor(() => expect(mocks.addToast).toHaveBeenCalledWith('error', '发送失败：ack lost'));
    const original = submissionOf(onSend);
    expect(view.input.value).toBe('  不能重复持久化  ');
    expect(view.getByText('note.txt')).toBeTruthy();
    view.send();
    await waitFor(() => expect(onSend).toHaveBeenCalledTimes(2));
    const retry = submissionOf(onSend, 1);
    expect(retry.externalId).toBe(original.externalId);
    expect(retry.payload).toBe(original.payload);
    expect(onSend.mock.calls[1]?.slice(0, 2)).toEqual(onSend.mock.calls[0]?.slice(0, 2));
    expect(HeldFileReader.reads).toHaveLength(1);
    await acknowledge(retry);
    expect(view.input.value).toBe('');
  });

  it('allocates a new identity for an edited draft even after the text is edited back to the same bytes', async () => {
    const onSend = vi.fn<Send>().mockResolvedValue(false);
    const view = await editor(onSend);
    view.type('same');
    view.send();
    await waitFor(() => expect(onSend).toHaveBeenCalledTimes(1));
    const original = submissionOf(onSend);
    view.type('different');
    view.type('same');
    view.send();
    await waitFor(() => expect(onSend).toHaveBeenCalledTimes(2));
    expect(submissionOf(onSend, 1).externalId).not.toBe(original.externalId);
    expect(view.input.value).toBe('same');
  });

  it.each(['error', 'abort', 'malformed'] as const)(
    'preserves text and files on an encoder %s, then permits retry',
    async (failure) => {
      const view = await editor();
      view.type('附件还在');
      view.attach();
      view.send();
      await act(async () => {
        const reader = HeldFileReader.reads[0];
        if (failure === 'error') reader?.fail();
        else if (failure === 'abort') reader?.abort();
        else reader?.complete('not-a-data-url');
      });
      await waitFor(() => expect(mocks.addToast).toHaveBeenCalledTimes(1));
      expect(view.onSend).not.toHaveBeenCalled();
      expect(view.input.value).toBe('附件还在');
      expect(view.getByText('note.txt')).toBeTruthy();
      expect(view.input.getAttribute('aria-busy')).toBe('false');
      view.send();
      expect(HeldFileReader.reads).toHaveLength(2);
      await act(async () => HeldFileReader.reads[1]?.complete());
      expect(view.onSend).toHaveBeenCalledTimes(1);
      expect(submissionOf(view.onSend).payload.text).toBe('附件还在');
    }
  );

  const changeTarget = {
    workspace: () => useWorkspaceStore.setState({ current: { ...workspace, id: 'workspace-b' } }),
    generation: () =>
      useWorkspaceStore.setState({ current: { ...workspace, product_data_generation: 'a-v2' } }),
    conversation: () => useConversationStore.setState({ activeId: 'conversation-b' }),
    newConversation: () => useConversationStore.setState({ newConversationEpoch: 1 }),
  };
  it.each(Object.keys(changeTarget) as Array<keyof typeof changeTarget>)(
    'does not clear a newer %s target on a late receipt or boolean',
    async (kind) => {
      const response = deferred<boolean>();
      const onSend = vi.fn<Send>().mockReturnValue(response.promise);
      const view = await editor(onSend);
      view.type('保留当前输入');
      view.send();
      await waitFor(() => expect(onSend).toHaveBeenCalledTimes(1));
      const original = submissionOf(onSend);
      act(changeTarget[kind]);
      await acknowledge(original);
      await act(async () => response.resolve(true));
      expect(view.input.value).toBe('保留当前输入');
      expect(view.queryByText('已收到')).toBeNull();
    }
  );

  it('does not retarget an input when the sidebar changes while attachment reading is held', async () => {
    const view = await editor();
    view.type('属于原会话');
    view.attach();
    view.send();
    act(() => useConversationStore.setState({ activeId: 'conversation-b' }));
    await act(async () => HeldFileReader.reads[0]?.complete());
    expect(view.onSend).not.toHaveBeenCalled();
    expect(view.input.value).toBe('属于原会话');
    view.send();
    await act(async () => HeldFileReader.reads[1]?.complete());
    expect(submissionOf(view.onSend).conversationId).toBe('conversation-b');
  });

  it('honors the original Hook turn fence even when the visible conversation still matches', async () => {
    const response = deferred<boolean>();
    const onSend = vi.fn<Send>().mockReturnValue(response.promise);
    const view = await editor(onSend);
    view.type('同会话新一轮的草稿');
    view.send();
    await waitFor(() => expect(onSend).toHaveBeenCalledTimes(1));
    await acknowledge(submissionOf(onSend), () => false);
    await act(async () => response.resolve(true));
    expect(view.input.value).toBe('同会话新一轮的草稿');
    expect(view.queryByText('已收到')).toBeNull();
  });

  it('cannot clear a remounted editor from a receipt owned by the unmounted instance', async () => {
    const response = deferred<boolean>();
    const onSend = vi.fn<Send>().mockReturnValue(response.promise);
    const old = await editor(onSend);
    old.type('旧实例');
    old.attach('image.png', 'image/png');
    old.send();
    await act(async () => HeldFileReader.reads[0]?.complete());
    const original = submissionOf(onSend);
    old.unmount();
    expect(URL.revokeObjectURL).toHaveBeenCalledWith('blob:owned-preview');
    const current = await editor();
    current.type('新实例');
    await acknowledge(original);
    await act(async () => response.resolve(true));
    expect(current.input.value).toBe('新实例');
    expect(current.queryByText('已收到')).toBeNull();
  });

  it('clears a normal first send only with the original sender proof, without fabricating a persisted notice', async () => {
    useConversationStore.setState({ activeId: null });
    const response = deferred<boolean>();
    const onSend = vi.fn<Send>().mockReturnValue(response.promise);
    const view = await editor(onSend);
    view.type('第一条');
    view.send();
    await waitFor(() => expect(onSend).toHaveBeenCalledTimes(1));
    act(() => useConversationStore.setState({ activeId: 'canonical-created' }));
    await act(async () => {
      await submissionOf(onSend).onDirectAccepted?.(() => true);
    });
    expect(view.input.value).toBe('');
    expect(view.queryByText('已收到')).toBeNull();
    view.type('新的草稿');
    await act(async () => response.resolve(true));
    expect(view.input.value).toBe('新的草稿');
  });

  it('does not admit the same first draft again when its canonical conversation is assigned before Started', async () => {
    useConversationStore.setState({ activeId: null });
    const response = deferred<boolean>();
    const onSend = vi.fn<Send>().mockReturnValue(response.promise);
    const view = await editor(onSend);
    view.type('同一首稿');
    view.send();
    await waitFor(() => expect(onSend).toHaveBeenCalledTimes(1));
    act(() => useConversationStore.setState({ activeId: 'canonical-created' }));
    view.send();
    fireEvent.click(view.getByRole('button', { name: '发送' }));
    await act(async () => {
      await Promise.resolve();
    });
    expect(onSend).toHaveBeenCalledTimes(1);
    expect(view.input.getAttribute('aria-busy')).toBe('true');
    await act(async () => {
      await submissionOf(onSend).onDirectAccepted?.(() => true);
    });
    expect(view.input.value).toBe('');
    await act(async () => response.resolve(true));
  });

  it('preserves the draft when a direct Started callback reports a stale owner', async () => {
    const response = deferred<boolean>();
    const onSend = vi.fn<Send>().mockReturnValue(response.promise);
    const view = await editor(onSend);
    view.type('仍需保留');
    view.send();
    await waitFor(() => expect(onSend).toHaveBeenCalledTimes(1));
    await act(async () => {
      await submissionOf(onSend).onDirectAccepted?.(() => false);
    });
    await act(async () => response.resolve(true));
    expect(view.input.value).toBe('仍需保留');
    expect(view.queryByText('已收到')).toBeNull();
  });

  it('keeps the ordinary boolean success fallback without treating it as a durable receipt', async () => {
    const onSend = vi.fn<Send>().mockResolvedValue(true);
    const view = await editor(onSend);
    view.type('普通发送');
    view.send();
    await waitFor(() => expect(view.input.value).toBe(''));
    expect(onSend).toHaveBeenCalledTimes(1);
    expect(view.queryByText('已收到')).toBeNull();
  });
});
