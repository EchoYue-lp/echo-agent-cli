// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import type { ToolExecution, ToolExecutionDetailManifest } from '../../types/api';
const mocks = vi.hoisted(() => ({ detail: vi.fn(), open: vi.fn() }));
vi.mock('../../api/endpoints', async (original) => ({
  ...(await original<typeof import('../../api/endpoints')>()),
  toolExecutionApi: { detail: mocks.detail },
}));
vi.mock('../../lib/openAgentConversation', () => ({ openAgentConversation: mocks.open }));
import { ToolExecutionGroup } from './ToolExecutionGroup';
import { useToolExecutionStore } from '../../stores/toolExecutionStore';
const tool: ToolExecution = {
  id: 'original-tool',
  call_id: 'call',
  workspace_id: 'w',
  conversation_id: 'source',
  owner: { kind: 'chat', message_id: 'assistant' },
  name: 'agent_message',
  args_preview: 'not an address source',
  status: 'succeeded',
  started_at: 1,
  finished_at: 2,
  detail_ref: 'original-detail',
};
const target = {
  type: 'conversation' as const,
  workspace_id: 'target-workspace',
  conversation_id: 'target-chat',
  workspace_generation: 'generation-1',
};
function manifest(): ToolExecutionDetailManifest {
  return {
    id: tool.id,
    status: 'succeeded',
    invocation: { name: tool.name, requested_name: tool.name, args: {}, requested_args: {} },
    output_bytes: 1,
    result: {
      kind: { kind_type: 'json' },
      success: true,
      output: 'not parsed',
      truncated: false,
      metadata: {},
      data: {
        operation: 'message',
        target,
        status: 'persisted',
        phase: 'persisted',
        outcome: null,
        message_id: 'original-message',
        duplicate: false,
        command_id: null,
        cursor: null,
        detail: null,
      },
    },
  };
}
beforeEach(() => {
  mocks.detail.mockReset();
  mocks.open.mockReset().mockResolvedValue(undefined);
  useToolExecutionStore.getState().clear();
  useToolExecutionStore.getState().ingest(tool);
});
afterEach(cleanup);
it('shows the exact typed send result outside collapsed tool details and only navigates on click', async () => {
  mocks.detail.mockResolvedValue(manifest());
  render(<ToolExecutionGroup toolIds={[tool.id]} />);
  const link = await screen.findByRole('button', { name: '已发送消息到会话 · target-chat' });
  expect(screen.getByText('persisted')).toBeDefined();
  expect(mocks.open).not.toHaveBeenCalled();
  expect(mocks.detail).toHaveBeenCalledExactlyOnceWith('w', 'original-detail');
  fireEvent.click(link);
  expect(mocks.open).toHaveBeenCalledExactlyOnceWith(target);
  expect(screen.queryByText('已读')).toBeNull();
});
it('resolves a cold transcript call to its distinct exact detail identity before showing the sent link', async () => {
  mocks.detail.mockResolvedValue(manifest());
  render(
    <ToolExecutionGroup
      toolIds={['call']}
      transcriptTools={[
        {
          id: 'call',
          workspaceId: 'w',
          conversationId: 'source',
          messageId: 'assistant',
          callId: 'call',
          name: 'agent_message',
        },
      ]}
    />
  );
  const link = await screen.findByRole('button', { name: '已发送消息到会话 · target-chat' });
  expect(mocks.detail).toHaveBeenCalledExactlyOnceWith('w', 'original-detail');
  fireEvent.click(link);
  expect(mocks.open).toHaveBeenCalledExactlyOnceWith(target);
});
it('does not attach another owner detail to the same cold transcript call', async () => {
  mocks.detail.mockResolvedValue(manifest());
  render(
    <ToolExecutionGroup
      toolIds={['call']}
      transcriptTools={[
        {
          id: 'call',
          workspaceId: 'w',
          conversationId: 'source',
          messageId: 'different-assistant',
          callId: 'call',
          name: 'agent_message',
        },
      ]}
    />
  );
  expect(screen.queryByRole('button', { name: /已发送消息到会话/ })).toBeNull();
  expect(mocks.detail).not.toHaveBeenCalled();
  expect(mocks.open).not.toHaveBeenCalled();
});
it('does not treat JSON-shaped output as a typed receipt or invent a clickable target', async () => {
  const detail = manifest();
  detail.result!.output = JSON.stringify(detail.result!.data);
  detail.result!.data = null;
  mocks.detail.mockResolvedValue(detail);
  render(<ToolExecutionGroup toolIds={[tool.id]} />);
  await waitFor(() => expect(mocks.detail).toHaveBeenCalledOnce());
  expect(screen.queryByRole('button', { name: /已发送消息到会话/ })).toBeNull();
  expect(mocks.open).not.toHaveBeenCalled();
});
it('keeps a terminal delivery failure reason and original tool details available', async () => {
  const detail = manifest();
  detail.result!.data = {
    ...(detail.result!.data as Record<string, unknown>),
    status: 'completed',
    phase: 'completed',
    outcome: 'failed',
    duplicate: true,
    detail: '原目标执行失败，原投递没有重放',
  };
  mocks.detail.mockResolvedValue(detail);
  render(<ToolExecutionGroup toolIds={[tool.id]} />);
  await screen.findByRole('button', { name: '消息投递失败 · target-chat' });
  expect(screen.queryByRole('button', { name: /^已发送消息到会话/ })).toBeNull();
  fireEvent.click(screen.getByText('消息工具详情'));
  expect(screen.getByText('原目标执行失败，原投递没有重放')).toBeDefined();
  expect(mocks.open).not.toHaveBeenCalled();
});
it('rejects a late detail response after the original binding changes', async () => {
  let resolve: (detail: ToolExecutionDetailManifest) => void = () => undefined;
  mocks.detail
    .mockReturnValueOnce(
      new Promise((done) => {
        resolve = done;
      })
    )
    .mockResolvedValue({ ...manifest(), id: 'successor' });
  const view = render(<ToolExecutionGroup toolIds={[tool.id]} />);
  await waitFor(() => expect(mocks.detail).toHaveBeenCalledOnce());
  act(() => {
    useToolExecutionStore
      .getState()
      .ingest({ ...tool, id: 'successor', call_id: 'next-call', detail_ref: 'next-detail' });
  });
  view.rerender(<ToolExecutionGroup toolIds={['successor']} />);
  await act(async () => {
    resolve(manifest());
  });
  expect(mocks.open).not.toHaveBeenCalled();
  // Only the successor component may publish its own detail; the old request cannot restore its row.
  expect(document.querySelectorAll('[data-agent-sent]')).toHaveLength(1);
});
