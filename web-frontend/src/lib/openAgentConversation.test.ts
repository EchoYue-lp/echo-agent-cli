// @vitest-environment jsdom
import { beforeEach, describe, expect, it, vi } from 'vitest';
const state = vi.hoisted(() => ({
  activeId: 'original',
  epoch: 0,
  draft: 0,
  workspace: 'a',
  generation: 'ga',
  opened: 0,
  load: vi.fn(),
  switchTo: vi.fn(),
  exit: vi.fn(),
}));
vi.mock('../stores/uiStore', () => ({
  useUiStore: {
    getState: () => ({
      navigationEpoch: state.epoch,
      beginNavigation: () => ++state.epoch,
    }),
  },
}));
vi.mock('../stores/workspaceViewStore', () => ({
  useWorkspaceViewStore: {
    getState: () => ({ openChat: () => state.opened++ }),
  },
}));
vi.mock('../stores/workspaceStore', () => ({
  useWorkspaceStore: {
    getState: () => ({
      current: {
        id: state.workspace,
        product_data_generation: state.generation,
      },
      workspaces: [
        { id: 'a', product_data_generation: 'ga' },
        { id: 'b', product_data_generation: 'gb' },
      ],
      switchTo: state.switchTo,
      exit: state.exit,
    }),
  },
}));
vi.mock('../stores/conversationStore', () => ({
  useConversationStore: {
    getState: () => ({
      activeId: state.activeId,
      workspaceId: state.workspace,
      newConversationEpoch: state.draft,
      loadConversation: state.load,
    }),
  },
}));
import { openAgentConversation } from './openAgentConversation';

beforeEach(() => {
  state.epoch = state.draft = state.opened = 0;
  state.workspace = 'a';
  state.generation = 'ga';
  state.activeId = 'original';
  state.load.mockReset().mockImplementation(async (id: string) => {
    state.activeId = id;
  });
  state.exit.mockReset();
  // Fenced switch detaches AFTER its asynchronous transition receipt, as the real store does.
  state.switchTo.mockReset().mockImplementation(async (id: string) => {
    await Promise.resolve();
    state.workspace = id;
    state.generation = id === 'b' ? 'gb' : 'ga';
    state.draft++;
  });
});
describe('normal target navigation', () => {
  it('opens the exact same-workspace conversation without sending or creating', async () => {
    await openAgentConversation({
      workspace_id: 'a',
      conversation_id: 'target',
      workspace_generation: 'ga',
    });
    expect(state.switchTo).not.toHaveBeenCalled();
    expect(state.load).toHaveBeenCalledOnce();
    expect(state.load.mock.calls[0]?.[0]).toBe('target');
    expect(state.opened).toBe(1);
  });
  it('survives the real fenced-switch detach epoch before loading another workspace conversation', async () => {
    await openAgentConversation({
      workspace_id: 'b',
      conversation_id: 'target-b',
      workspace_generation: 'gb',
    });
    expect(state.switchTo).toHaveBeenCalledExactlyOnceWith('b', undefined, 'gb');
    expect(state.draft).toBe(1);
    expect(state.load.mock.calls[0]?.[0]).toBe('target-b');
    expect(state.load.mock.calls[0]?.[1]()).toBe(true);
    expect(state.opened).toBe(1);
  });
  it('refuses a reused workspace generation or missing original generation', async () => {
    await expect(
      openAgentConversation({
        workspace_id: 'b',
        conversation_id: 'target',
        workspace_generation: 'old',
      })
    ).rejects.toThrow('代际');
    await expect(
      openAgentConversation({
        workspace_id: 'b',
        conversation_id: 'target',
        workspace_generation: null,
      })
    ).rejects.toThrow('代际');
    expect(state.switchTo).not.toHaveBeenCalled();
    expect(state.load).not.toHaveBeenCalled();
  });
  it('does not adopt a click superseded during the asynchronous workspace transition', async () => {
    state.switchTo.mockImplementation(async () => {
      await Promise.resolve();
      state.epoch++;
      state.workspace = 'b';
      state.generation = 'gb';
      state.draft++;
    });
    await openAgentConversation({
      workspace_id: 'b',
      conversation_id: 'target',
      workspace_generation: 'gb',
    });
    expect(state.load).not.toHaveBeenCalled();
    expect(state.opened).toBe(0);
  });
  it('does not open a target if its workspace disappears or transition fails', async () => {
    await expect(
      openAgentConversation({
        workspace_id: 'gone',
        conversation_id: 'target',
        workspace_generation: 'g',
      })
    ).rejects.toThrow('不存在');
    state.switchTo.mockRejectedValue(new Error('transition failed'));
    await expect(
      openAgentConversation({
        workspace_id: 'b',
        conversation_id: 'target',
        workspace_generation: 'gb',
      })
    ).rejects.toThrow('transition failed');
    expect(state.load).not.toHaveBeenCalled();
    expect(state.opened).toBe(0);
  });
});

it('opens a same-global typed target using the original global generation token', async () => {
  state.workspace = 'global';
  state.generation = 'global';
  await openAgentConversation({
    workspace_id: 'global',
    conversation_id: 'global-target',
    workspace_generation: 'global',
  });
  expect(state.exit).not.toHaveBeenCalled();
  expect(state.load.mock.calls[0]?.[0]).toBe('global-target');
  expect(state.opened).toBe(1);
});
it('exits a workspace normally before opening a typed global target', async () => {
  state.exit.mockImplementation(async () => {
    await Promise.resolve();
    state.workspace = 'global';
    state.generation = 'global';
    state.draft++;
  });
  await openAgentConversation({
    workspace_id: 'global',
    conversation_id: 'global-target',
    workspace_generation: 'global',
  });
  expect(state.exit).toHaveBeenCalledOnce();
  expect(state.load.mock.calls[0]?.[0]).toBe('global-target');
  expect(state.opened).toBe(1);
});
it('rejects a wrong global generation without exiting or loading', async () => {
  await expect(
    openAgentConversation({
      workspace_id: 'global',
      conversation_id: 'target',
      workspace_generation: 'old',
    })
  ).rejects.toThrow('代际');
  expect(state.exit).not.toHaveBeenCalled();
  expect(state.load).not.toHaveBeenCalled();
});

it('does not claim the target opened when the normal loader retains the previous conversation on failure', async () => {
  state.load.mockResolvedValue(undefined);
  await openAgentConversation({
    workspace_id: 'a',
    conversation_id: 'failed-target',
    workspace_generation: 'ga',
  });
  expect(state.activeId).toBe('original');
  expect(state.opened).toBe(0);
});
