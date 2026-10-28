// @vitest-environment jsdom
import { act, cleanup, renderHook } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
const source = vi.hoisted(() => ({
  workspace: { current: { id: 'w', product_data_generation: 'g1' } },
  conversation: { activeId: 'c', newConversationEpoch: 1 },
  read: vi.fn(),
  observe: vi.fn(),
  error: vi.fn(),
}));
vi.mock('../api/endpoints', () => ({
  conversationApi: { agentInputs: source.read },
}));
vi.mock('../stores/workspaceStore', () => ({
  useWorkspaceStore: Object.assign(
    (selector: (state: typeof source.workspace) => unknown) => selector(source.workspace),
    { getState: () => source.workspace }
  ),
}));
vi.mock('../stores/conversationStore', () => ({
  useConversationStore: Object.assign(
    (selector: (state: typeof source.conversation) => unknown) => selector(source.conversation),
    { getState: () => source.conversation }
  ),
}));
vi.mock('../stores/chatStore', () => ({
  useChatStore: {
    getState: () => ({ observeAgentInputs: source.observe, noteAgentInputReadError: source.error }),
  },
}));
import { useAgentCommunicationInputs } from './useAgentCommunicationInputs';
import type { AgentCommunicationInput } from '../lib/agentCommunication';
function deferred() {
  let resolve: (value: AgentCommunicationInput[]) => void = () => undefined;
  const promise = new Promise<AgentCommunicationInput[]>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}
beforeEach(() => {
  vi.useFakeTimers();
  source.workspace.current = { id: 'w', product_data_generation: 'g1' };
  source.conversation = { activeId: 'c', newConversationEpoch: 1 };
  source.read.mockReset();
  source.observe.mockReset();
  source.error.mockReset();
});
afterEach(() => {
  cleanup();
  vi.useRealTimers();
});
describe('durable inbox observer binding', () => {
  it('refreshes live without overlapping requests and stops after unmount', async () => {
    const first = deferred();
    source.read.mockReturnValueOnce(first.promise).mockResolvedValue([]);
    const view = renderHook(() => useAgentCommunicationInputs('w', 'c'));
    await act(async () => {
      await vi.advanceTimersByTimeAsync(4000);
    });
    expect(source.read).toHaveBeenCalledTimes(1);
    await act(async () => {
      first.resolve([]);
    });
    expect(source.observe).toHaveBeenCalledTimes(1);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(1000);
    });
    expect(source.read).toHaveBeenCalledTimes(2);
    view.unmount();
    await vi.advanceTimersByTimeAsync(3000);
    expect(source.read).toHaveBeenCalledTimes(2);
  });
  it('rebinds same IDs after generation/epoch changes and rejects the late old response', async () => {
    const old = deferred();
    const fresh = deferred();
    source.read.mockReturnValueOnce(old.promise).mockReturnValueOnce(fresh.promise);
    const view = renderHook(() => useAgentCommunicationInputs('w', 'c'));
    source.workspace.current.product_data_generation = 'g2';
    source.conversation.newConversationEpoch++;
    view.rerender();
    expect(source.read).toHaveBeenCalledTimes(2);
    await act(async () => {
      old.resolve([]);
    });
    expect(source.observe).not.toHaveBeenCalled();
    await act(async () => {
      fresh.resolve([]);
    });
    expect(source.read.mock.calls[1]?.[0]).toEqual({
      workspaceId: 'w',
      workspaceGeneration: 'g2',
    });
    expect(source.observe.mock.calls[0]?.[3]).toBe(JSON.stringify(['w', 'g2', 'c', 2]));
  });
  it('rejects an old A response after A→B→A and keeps only the new A observation', async () => {
    const oldA = deferred();
    const b = deferred();
    const newA = deferred();
    source.read
      .mockReturnValueOnce(oldA.promise)
      .mockReturnValueOnce(b.promise)
      .mockReturnValueOnce(newA.promise);
    const view = renderHook(({ id }) => useAgentCommunicationInputs('w', id), {
      initialProps: { id: 'c' },
    });
    source.conversation = { activeId: 'b', newConversationEpoch: 2 };
    view.rerender({ id: 'b' });
    source.conversation = { activeId: 'c', newConversationEpoch: 3 };
    view.rerender({ id: 'c' });
    await act(async () => {
      oldA.resolve([]);
      b.resolve([]);
    });
    expect(source.observe).not.toHaveBeenCalled();
    await act(async () => {
      newA.resolve([]);
    });
    expect(source.observe).toHaveBeenCalledTimes(1);
  });
  it('does not erase visible observations or infer empty/success from a read failure', async () => {
    source.read.mockRejectedValue(new Error('read failed'));
    renderHook(() => useAgentCommunicationInputs('w', 'c'));
    await act(async () => {
      await Promise.resolve();
    });
    expect(source.observe).not.toHaveBeenCalled();
    expect(source.error).toHaveBeenCalledOnce();
  });
});
