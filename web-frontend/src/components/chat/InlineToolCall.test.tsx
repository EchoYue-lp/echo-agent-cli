// @vitest-environment jsdom
import { beforeEach, afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, render, screen, fireEvent, act, waitFor } from '@testing-library/react';
import { toolExecutionIdsForOwner, useToolExecutionStore } from '../../stores/toolExecutionStore';
import type { ToolExecution, TranscriptToolRecord } from '../../types/api';
import { InlineToolCall, toolSummaryText } from './InlineToolCall';

const detailApi = vi.hoisted(() => ({ detail: vi.fn(), readOutput: vi.fn() }));
vi.mock('../../api/endpoints', () => ({ toolExecutionApi: detailApi }));

function summary(overrides: Partial<ToolExecution> = {}): ToolExecution {
  return {
    id: 'detail-1',
    workspace_id: 'workspace-1',
    call_id: 'call-1',
    owner: { kind: 'chat', message_id: 'message-1' },
    conversation_id: 'conversation-1',
    run_id: 'run-1',
    name: 'shell',
    args_preview: '{"command":"printf hello"}',
    status: 'succeeded',
    started_at: 1_000,
    finished_at: 2_000,
    duration_ms: 1_000,
    detail_ref: 'detail-1',
    ...overrides,
  };
}

describe('InlineToolCall', () => {
  beforeEach(() => {
    useToolExecutionStore.getState().clear();
  });

  it('keeps only summaries in the normalized store', () => {
    useToolExecutionStore.getState().ingest(summary());
    const tool = useToolExecutionStore.getState().tools['detail-1'];

    expect(tool).toEqual(summary());
    expect(toolSummaryText(tool?.name ?? '', tool?.args_preview ?? '')).toBe(
      'shell · {"command":"printf hello"}'
    );
  });

  it('uses the same row for a running subagent-owned tool', () => {
    useToolExecutionStore.getState().ingest(
      summary({
        id: 'detail-subagent',
        detail_ref: 'detail-subagent',
        owner: { kind: 'subagent', subagent_run_id: 'task-1:1' },
        name: 'read_file',
        args_preview: '{"path":"README.md"}',
        status: 'running',
        finished_at: null,
        duration_ms: null,
      })
    );
    const ownerIds = useToolExecutionStore.getState().idsByOwner['subagent:run-1:task-1:1'];

    expect(ownerIds).toEqual(['detail-subagent']);
    expect(useToolExecutionStore.getState().tools['detail-subagent']?.status).toBe('running');
  });

  it('returns a stable empty snapshot for an owner without tools', () => {
    const selectMissingOwner = () =>
      toolExecutionIdsForOwner(
        useToolExecutionStore.getState().idsByOwner,
        'subagent:without-tools'
      );

    const firstSnapshot = selectMissingOwner();
    const secondSnapshot = selectMissingOwner();

    expect(firstSnapshot).toBe(secondSnapshot);
    expect(firstSnapshot).toEqual([]);
  });
});

afterEach(cleanup);

const transcript: TranscriptToolRecord = {
  id: 'transcript-only',
  workspaceId: 'workspace-1',
  conversationId: 'conversation-1',
  messageId: 'message-1',
  callId: 'call-1',
  name: 'shell',
  callType: 'function',
  requestedArguments: '{"command":"原始参数"}',
  recordedOutput: '[Error] 只是记录，不推断失败',
};

describe('InlineToolCall canonical transcript display', () => {
  beforeEach(() => useToolExecutionStore.getState().clear());

  it('expands original records without inventing an execution status or duration', () => {
    render(<InlineToolCall toolId={transcript.id} transcript={transcript} />);
    fireEvent.click(screen.getByRole('button', { name: /shell.*状态未恢复/ }));
    expect(screen.getByText('原始请求参数')).toBeDefined();
    expect(screen.getByText(transcript.requestedArguments ?? '')).toBeDefined();
    expect(screen.getByText(transcript.recordedOutput ?? '')).toBeDefined();
    expect(screen.queryByText('failed', { exact: true })).toBeNull();
    expect(screen.queryByText(/\d+\.\d+s/)).toBeNull();
  });

  it('reads real running and terminal states without replacing them with fallback data', () => {
    const running = summary({
      status: 'running',
      finished_at: null,
      duration_ms: null,
      detail_ref: '',
    });
    useToolExecutionStore.getState().ingest(running);
    render(<InlineToolCall toolId={transcript.id} transcript={transcript} />);
    expect(screen.getByText('running', { exact: true })).toBeDefined();
    act(() =>
      useToolExecutionStore
        .getState()
        .ingest({ ...running, status: 'cancelled', finished_at: 3000, duration_ms: 2000 })
    );
    expect(screen.getByText('cancelled', { exact: true })).toBeDefined();
    expect(screen.queryByText('状态未恢复', { exact: true })).toBeNull();
    expect(useToolExecutionStore.getState().tools['detail-1']?.status).toBe('cancelled');
  });

  it('ignores an unrelated summary even when its UI key equals the fallback key', () => {
    useToolExecutionStore
      .getState()
      .ingest(summary({ id: transcript.id, owner: { kind: 'chat', message_id: 'wrong-owner' } }));
    render(<InlineToolCall toolId={transcript.id} transcript={transcript} />);
    expect(screen.getByText('状态未恢复', { exact: true })).toBeDefined();
    expect(screen.queryByText('success', { exact: true })).toBeNull();
  });
});

it('hides an already loaded detail when the original owner/call identity later becomes ambiguous', async () => {
  useToolExecutionStore.getState().clear();
  detailApi.detail.mockResolvedValue({
    id: 'detail-1',
    invocation: { name: 'shell', args: {} },
    status: 'succeeded',
    result: null,
    output_bytes: 10,
  });
  detailApi.readOutput.mockResolvedValue({
    chunks: [{ channel: 'result', text: '精确来源的详情正文' }],
    complete: true,
    next_cursor: null,
  });
  useToolExecutionStore.getState().ingest(summary());
  render(<InlineToolCall toolId={transcript.id} transcript={transcript} />);
  fireEvent.click(screen.getByRole('button', { name: /shell.*success/ }));
  await screen.findByText('精确来源的详情正文');
  act(() =>
    useToolExecutionStore
      .getState()
      .ingest(summary({ id: 'other-detail', detail_ref: 'other-detail' }))
  );
  expect(screen.getByText('状态未恢复', { exact: true })).toBeDefined();
  expect(screen.queryByText('精确来源的详情正文')).toBeNull();
  expect(screen.queryByText('success', { exact: true })).toBeNull();
});

function testManifest(id: string) {
  return {
    id,
    invocation: { name: 'shell', args: {} },
    status: 'succeeded',
    result: null,
    output_bytes: 4,
  };
}
function testPage(text: string) {
  return { chunks: [{ channel: 'result', text }], complete: true, next_cursor: null };
}
function detailSummary(id: string) {
  return summary({ id, detail_ref: id });
}

describe('InlineToolCall bound detail request isolation', () => {
  beforeEach(() => {
    useToolExecutionStore.getState().clear();
    detailApi.detail.mockReset();
    detailApi.readOutput.mockReset();
  });

  it('loads B after an already completed A when the same call changes detail identity', async () => {
    detailApi.detail.mockImplementation(async (_workspaceId, ref) => testManifest(ref));
    detailApi.readOutput.mockImplementation(async (_workspaceId, ref) =>
      testPage(ref === 'detail-A' ? 'A已加载详情' : 'B新详情')
    );
    useToolExecutionStore.getState().replaceAll([detailSummary('detail-A')]);
    render(<InlineToolCall toolId={transcript.id} transcript={transcript} />);
    fireEvent.click(screen.getByRole('button', { name: /shell.*success/ }));
    await screen.findByText('A已加载详情');
    act(() => useToolExecutionStore.getState().replaceAll([detailSummary('detail-B')]));
    await screen.findByText('B新详情');
    expect(screen.queryByText('A已加载详情')).toBeNull();
    expect(detailApi.detail).toHaveBeenCalledWith('workspace-1', 'detail-B');
  });

  it.each(['resolve', 'reject'] as const)(
    'ignores a late A %s across A→unknown→B and lets B load normally',
    async (outcome) => {
      let resolveA: (value: ReturnType<typeof testManifest>) => void = () => undefined;
      let rejectA: (reason: Error) => void = () => undefined;
      const manifestA = new Promise<ReturnType<typeof testManifest>>((resolve, reject) => {
        resolveA = resolve;
        rejectA = reject;
      });
      detailApi.detail.mockImplementation((_workspaceId, ref) =>
        ref === 'detail-A' ? manifestA : Promise.resolve(testManifest(ref))
      );
      detailApi.readOutput.mockImplementation(async (_workspaceId, ref) =>
        testPage(ref === 'detail-A' ? 'A迟到详情' : 'B正常详情')
      );
      useToolExecutionStore.getState().replaceAll([detailSummary('detail-A')]);
      render(<InlineToolCall toolId={transcript.id} transcript={transcript} />);
      fireEvent.click(screen.getByRole('button', { name: /shell.*success/ }));
      await waitFor(() => expect(detailApi.detail).toHaveBeenCalledTimes(1));
      act(() => useToolExecutionStore.getState().replaceAll([]));
      expect(screen.getByText('状态未恢复', { exact: true })).toBeDefined();
      act(() => useToolExecutionStore.getState().replaceAll([detailSummary('detail-B')]));
      try {
        await screen.findByText('B正常详情');
      } finally {
        await act(async () => {
          if (outcome === 'resolve') resolveA(testManifest('detail-A'));
          else rejectA(new Error('A旧请求错误'));
        });
      }
      expect(screen.getByText('B正常详情')).toBeDefined();
      expect(screen.queryByText('A迟到详情')).toBeNull();
      expect(screen.queryByText('A旧请求错误')).toBeNull();
    }
  );

  it('loads the first detail when an unknown row gains an exact summary', async () => {
    detailApi.detail.mockResolvedValue(testManifest('detail-B'));
    detailApi.readOutput.mockResolvedValue(testPage('从未知恢复B'));
    render(<InlineToolCall toolId={transcript.id} transcript={transcript} />);
    fireEvent.click(screen.getByRole('button', { name: /shell.*状态未恢复/ }));
    expect(detailApi.detail).not.toHaveBeenCalled();
    act(() => useToolExecutionStore.getState().replaceAll([detailSummary('detail-B')]));
    await screen.findByText('从未知恢复B');
  });

  it('reloads details when the same id and reference bind to another original owner', async () => {
    detailApi.detail.mockResolvedValue(testManifest('same-detail'));
    detailApi.readOutput
      .mockResolvedValueOnce(testPage('owner-A详情'))
      .mockResolvedValueOnce(testPage('owner-B详情'));
    useToolExecutionStore.getState().replaceAll([detailSummary('same-detail')]);
    const { rerender } = render(<InlineToolCall toolId={transcript.id} transcript={transcript} />);
    fireEvent.click(screen.getByRole('button', { name: /shell.*success/ }));
    await screen.findByText('owner-A详情');
    act(() =>
      useToolExecutionStore.getState().replaceAll([
        summary({
          id: 'same-detail',
          detail_ref: 'same-detail',
          owner: { kind: 'chat', message_id: 'owner-B' },
        }),
      ])
    );
    rerender(
      <InlineToolCall toolId={transcript.id} transcript={{ ...transcript, messageId: 'owner-B' }} />
    );
    await screen.findByText('owner-B详情');
    expect(screen.queryByText('owner-A详情')).toBeNull();
  });
});

it.each(['resolve', 'reject'] as const)(
  'does not let an old A %s finish or fail the in-flight B request',
  async (outcome) => {
    useToolExecutionStore.getState().clear();
    detailApi.detail.mockReset();
    detailApi.readOutput.mockReset();
    let resolveA: (value: ReturnType<typeof testManifest>) => void = () => undefined;
    let rejectA: (reason: Error) => void = () => undefined;
    let resolveB: (value: ReturnType<typeof testManifest>) => void = () => undefined;
    const a = new Promise<ReturnType<typeof testManifest>>((resolve, reject) => {
      resolveA = resolve;
      rejectA = reject;
    });
    const b = new Promise<ReturnType<typeof testManifest>>((resolve) => {
      resolveB = resolve;
    });
    detailApi.detail.mockImplementation((_workspaceId, ref) => (ref === 'detail-A' ? a : b));
    detailApi.readOutput.mockImplementation(async (_workspaceId, ref) =>
      testPage(ref === 'detail-A' ? 'A旧详情' : 'B后到详情')
    );
    useToolExecutionStore.getState().replaceAll([detailSummary('detail-A')]);
    render(<InlineToolCall toolId={transcript.id} transcript={transcript} />);
    fireEvent.click(screen.getByRole('button', { name: /shell.*success/ }));
    await waitFor(() => expect(detailApi.detail).toHaveBeenCalledTimes(1));
    act(() => useToolExecutionStore.getState().replaceAll([detailSummary('detail-B')]));
    await waitFor(() => expect(detailApi.detail).toHaveBeenCalledTimes(2));
    await act(async () => {
      if (outcome === 'resolve') resolveA(testManifest('detail-A'));
      else rejectA(new Error('A旧错误不属于B'));
    });
    expect(screen.getByText('正在加载完整信息...')).toBeDefined();
    expect(screen.queryByText('A旧错误不属于B')).toBeNull();
    expect(screen.queryByText('A旧详情')).toBeNull();
    await act(async () => resolveB(testManifest('detail-B')));
    expect(await screen.findByText('B后到详情')).toBeDefined();
  }
);

it('retries only the current bound detail after a real current request failure', async () => {
  useToolExecutionStore.getState().clear();
  detailApi.detail.mockReset();
  detailApi.readOutput.mockReset();
  detailApi.detail
    .mockRejectedValueOnce(new Error('当前详情读取失败'))
    .mockResolvedValueOnce(testManifest('detail-B'));
  detailApi.readOutput.mockResolvedValue(testPage('B重试成功详情'));
  useToolExecutionStore.getState().replaceAll([detailSummary('detail-B')]);
  render(<InlineToolCall toolId={transcript.id} transcript={transcript} />);
  fireEvent.click(screen.getByRole('button', { name: /shell.*success/ }));
  await screen.findByText('当前详情读取失败');
  expect(detailApi.detail).toHaveBeenCalledTimes(1);
  fireEvent.click(screen.getByRole('button', { name: '重试加载' }));
  await screen.findByText('B重试成功详情');
  expect(screen.queryByText('当前详情读取失败')).toBeNull();
  expect(detailApi.detail).toHaveBeenCalledTimes(2);
  expect(detailApi.detail).toHaveBeenLastCalledWith('workspace-1', 'detail-B');
});
