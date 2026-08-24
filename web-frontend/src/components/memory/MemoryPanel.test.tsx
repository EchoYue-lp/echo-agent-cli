// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const api = vi.hoisted(() => ({
  list: vi.fn(),
  search: vi.fn(),
  namespaces: vi.fn(),
  add: vi.fn(),
  delete: vi.fn(),
  status: vi.fn(),
  preview: vi.fn(),
}));

vi.mock('../../api/endpoints', () => ({
  memoryApi: {
    list: api.list,
    namespaces: api.namespaces,
    search: api.search,
    add: api.add,
    delete: api.delete,
  },
  autoMemoryApi: {
    status: api.status,
    toggle: vi.fn(),
    preview: api.preview,
    extract: vi.fn(),
  },
}));

vi.mock('../../stores/workspaceStore', async () => {
  const { create } = await import('zustand');
  return {
    useWorkspaceStore: create(() => ({ current: workspace('workspace-a') })),
  };
});

import { useWorkspaceStore } from '../../stores/workspaceStore';
import type { Workspace } from '../../api/endpoints';
import { MemoryPanel } from './MemoryPanel';

function workspace(id: string): Workspace {
  return {
    id,
    name: id,
    root: `/${id}`,
    kind: { type: 'general' },
    metadata: { tags: [] },
    created_at: '2026-08-24T00:00:00Z',
    last_active: '2026-08-24T00:00:00Z',
  };
}

function deferred<T>() {
  let resolve: (value: T) => void = () => {};
  let reject: (error: Error) => void = () => {};
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, reject, resolve };
}

describe('MemoryPanel workspace projection', () => {
  afterEach(cleanup);

  beforeEach(() => {
    api.list.mockReset();
    api.search.mockReset();
    api.namespaces.mockReset();
    api.add.mockReset();
    api.delete.mockReset();
    api.status.mockReset();
    api.preview.mockReset();
    api.namespaces.mockResolvedValue({ namespaces: [['agent', 'memories']] });
    api.status.mockResolvedValue({
      enabled: true,
      observation_count: 0,
      config: { min_confidence: 0.7, max_per_session: 10 },
    });
    useWorkspaceStore.setState({ current: workspace('workspace-a') });
  });

  it('does not let an A1 add response close or refresh the A2 form', async () => {
    const addA1 = deferred<Record<string, unknown>>();
    api.list.mockResolvedValue([]);
    api.add.mockReturnValue(addA1.promise);

    render(<MemoryPanel />);
    fireEvent.click(screen.getByRole('button', { name: '添加记忆' }));
    fireEvent.change(screen.getByPlaceholderText('键'), { target: { value: 'a1-key' } });
    fireEvent.change(screen.getByPlaceholderText('值'), { target: { value: 'a1-value' } });
    fireEvent.click(screen.getByRole('button', { name: '添加' }));
    await waitFor(() => expect(api.add).toHaveBeenCalled());

    act(() => useWorkspaceStore.setState({ current: workspace('workspace-b') }));
    act(() => useWorkspaceStore.setState({ current: workspace('workspace-a') }));
    fireEvent.change(screen.getByPlaceholderText('键'), { target: { value: 'a2-key' } });
    const listCallsBeforeOldResponse = api.list.mock.calls.length;
    await act(async () => addA1.resolve({ success: true }));

    expect(screen.getByPlaceholderText('键')).toHaveProperty('value', 'a2-key');
    expect(api.list).toHaveBeenCalledTimes(listCallsBeforeOldResponse);
  });

  it('does not let an A1 delete response refresh an A2 projection', async () => {
    const deleteA1 = deferred<Record<string, unknown>>();
    api.delete.mockReturnValue(deleteA1.promise);
    api.list
      .mockResolvedValueOnce([
        {
          namespace: 'agent/memories',
          key: 'shared-key',
          value: 'A1 value',
          created_at: 1,
          updated_at: 1,
        },
      ])
      .mockResolvedValueOnce([])
      .mockResolvedValue([
        {
          namespace: 'agent/memories',
          key: 'shared-key',
          value: 'A2 value',
          created_at: 2,
          updated_at: 2,
        },
      ]);

    render(<MemoryPanel />);
    expect(await screen.findByText('A1 value')).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: '删除 shared-key' }));
    await waitFor(() => expect(api.delete).toHaveBeenCalled());
    act(() => useWorkspaceStore.setState({ current: workspace('workspace-b') }));
    act(() => useWorkspaceStore.setState({ current: workspace('workspace-a') }));
    expect(await screen.findByText('A2 value')).toBeTruthy();
    const listCallsBeforeOldResponse = api.list.mock.calls.length;
    await act(async () => deleteA1.resolve({ success: true }));

    expect(screen.getByText('A2 value')).toBeTruthy();
    expect(api.list).toHaveBeenCalledTimes(listCallsBeforeOldResponse);
  });

  it('rejects an old A generation after A to B to A and same-scope request reordering', async () => {
    const oldA = deferred<Array<Record<string, unknown>>>();
    const listB = deferred<Array<Record<string, unknown>>>();
    const newA = deferred<Array<Record<string, unknown>>>();
    api.list
      .mockReturnValueOnce(oldA.promise)
      .mockReturnValueOnce(listB.promise)
      .mockReturnValueOnce(newA.promise);
    api.search.mockResolvedValue([
      {
        namespace: 'agent/memories',
        key: 'search-new',
        value: 'new search result',
        created_at: 3,
        updated_at: 3,
      },
    ]);

    render(<MemoryPanel />);
    act(() => useWorkspaceStore.setState({ current: workspace('workspace-b') }));
    act(() => useWorkspaceStore.setState({ current: workspace('workspace-a') }));
    await act(async () => {
      newA.resolve([
        {
          namespace: 'agent/memories',
          key: 'a-new',
          value: 'new A generation',
          created_at: 2,
          updated_at: 2,
        },
      ]);
    });
    expect(await screen.findByText('new A generation')).toBeTruthy();

    await act(async () => {
      oldA.resolve([
        {
          namespace: 'agent/memories',
          key: 'a-old',
          value: 'old A generation',
          created_at: 1,
          updated_at: 1,
        },
      ]);
    });
    expect(screen.queryByText('old A generation')).toBeNull();
    expect(screen.getByText('new A generation')).toBeTruthy();

    const searchInput = screen.getByPlaceholderText('搜索...');
    fireEvent.change(searchInput, { target: { value: 'new' } });
    fireEvent.keyDown(searchInput, { key: 'Enter' });
    await waitFor(() => expect(api.search).toHaveBeenCalled());
    expect(await screen.findByText('new search result')).toBeTruthy();
    await act(async () => listB.resolve([]));
    expect(screen.getByText('new search result')).toBeTruthy();
  });

  it('does not let an older list overwrite a newer same-workspace search', async () => {
    const olderList = deferred<Array<Record<string, unknown>>>();
    api.list.mockReturnValue(olderList.promise);
    api.search.mockResolvedValue([
      {
        namespace: 'agent/memories',
        key: 'search-newer',
        value: 'newer scoped search',
        created_at: 2,
        updated_at: 2,
      },
    ]);

    render(<MemoryPanel />);
    const searchInput = screen.getByPlaceholderText('搜索...');
    fireEvent.change(searchInput, { target: { value: 'fact' } });
    fireEvent.keyDown(searchInput, { key: 'Enter' });
    expect(await screen.findByText('newer scoped search')).toBeTruthy();

    await act(async () => {
      olderList.resolve([
        {
          namespace: 'agent/memories',
          key: 'list-older',
          value: 'older list result',
          created_at: 1,
          updated_at: 1,
        },
      ]);
    });
    expect(screen.queryByText('older list result')).toBeNull();
    expect(screen.getByText('newer scoped search')).toBeTruthy();
  });

  it('rejects stale auto status and preview results across scope generations', async () => {
    const oldStatus = deferred<Record<string, unknown>>();
    const oldPreview = deferred<Record<string, unknown>>();
    api.list.mockResolvedValue([]);
    api.status.mockReturnValueOnce(oldStatus.promise).mockResolvedValue({
      enabled: true,
      observation_count: 2,
      config: { enabled: true, min_confidence: 0.7, max_per_session: 10, categories: [] },
    });
    api.preview.mockReturnValueOnce(oldPreview.promise).mockResolvedValue({
      count: 1,
      observations: [{ category: 'Project', text: 'new preview', confidence: 0.9, source_turn: 2 }],
    });

    render(<MemoryPanel />);
    act(() => useWorkspaceStore.setState({ current: workspace('workspace-b') }));
    act(() => useWorkspaceStore.setState({ current: workspace('workspace-a') }));
    expect(await screen.findByText(/候选 2/)).toBeTruthy();
    await act(async () => {
      oldStatus.resolve({
        enabled: true,
        observation_count: 9,
        config: { enabled: true, min_confidence: 0.7, max_per_session: 10, categories: [] },
      });
    });
    expect(screen.queryByText(/候选 9/)).toBeNull();

    fireEvent.click(screen.getByRole('button', { name: '预览' }));
    act(() => useWorkspaceStore.setState({ current: workspace('workspace-b') }));
    act(() => useWorkspaceStore.setState({ current: workspace('workspace-a') }));
    await screen.findByText(/候选 2/);
    fireEvent.click(screen.getByRole('button', { name: '预览' }));
    expect(await screen.findByText('new preview')).toBeTruthy();
    await act(async () => {
      oldPreview.resolve({
        count: 1,
        observations: [
          { category: 'Project', text: 'old preview', confidence: 0.8, source_turn: 1 },
        ],
      });
    });
    expect(screen.queryByText('old preview')).toBeNull();
    expect(screen.getByText('new preview')).toBeTruthy();
  });

  it('hides workspace A data while workspace B is pending and after B fails', async () => {
    let rejectWorkspaceB: (error: Error) => void = () => {};
    api.list.mockImplementation((workspaceId: string) => {
      if (workspaceId === 'workspace-a') {
        return Promise.resolve([
          {
            namespace: 'agent/memories',
            key: 'fact-a',
            value: 'workspace A fact',
            created_at: 1,
            updated_at: 1,
          },
        ]);
      }
      return new Promise((_, reject) => {
        rejectWorkspaceB = reject;
      });
    });

    render(<MemoryPanel />);
    expect(await screen.findByText('workspace A fact')).toBeTruthy();

    act(() => {
      useWorkspaceStore.setState({ current: workspace('workspace-b') });
    });
    expect(screen.queryByText('workspace A fact')).toBeNull();

    act(() => rejectWorkspaceB(new Error('workspace B unavailable')));
    await waitFor(() => expect(api.list).toHaveBeenCalledWith('workspace-b', undefined));
    expect(screen.queryByText('workspace A fact')).toBeNull();
  });
});
