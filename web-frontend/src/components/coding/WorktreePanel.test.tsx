// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const api = vi.hoisted(() => ({
  list: vi.fn(),
  listUnattended: vi.fn(),
  cleanupUnattended: vi.fn(),
}));

vi.mock('../../api/endpoints', () => ({
  worktreeApi: {
    list: api.list,
    listUnattended: api.listUnattended,
    create: vi.fn(),
    remove: vi.fn(),
    mergeUnattended: vi.fn(),
    discardUnattended: vi.fn(),
    cleanupUnattended: api.cleanupUnattended,
  },
}));

vi.mock('../../stores/workspaceStore', async () => {
  const { create } = await import('zustand');
  return {
    useWorkspaceStore: create(() => ({ current: { id: 'workspace-a' } })),
  };
});

import { useWorkspaceStore } from '../../stores/workspaceStore';
import type { Workspace } from '../../api/endpoints';
import { WorktreePanel } from './WorktreePanel';

function workspace(id: string): Workspace {
  return {
    id,
    name: id,
    root: `/${id}`,
    kind: { type: 'general' },
    metadata: { tags: [] },
    product_data_generation: 'workspace-1-generation',
    created_at: '2026-08-24T00:00:00Z',
    last_active: '2026-08-24T00:00:00Z',
  };
}

function deferred<T>() {
  let resolve: (value: T) => void = () => {};
  const promise = new Promise<T>((resolvePromise) => {
    resolve = resolvePromise;
  });
  return { promise, resolve };
}

describe('WorktreePanel workspace projection', () => {
  afterEach(cleanup);

  beforeEach(() => {
    api.list.mockReset();
    api.listUnattended.mockReset();
    api.cleanupUnattended.mockReset();
    useWorkspaceStore.setState({ current: workspace('workspace-a') });
  });

  it('does not carry A cleanup busy state or warning into workspace B', async () => {
    const cleanupA = deferred<{
      removed: string[];
      unlocked: string[];
      kept: string[];
      errors: string[];
    }>();
    api.list.mockResolvedValue([]);
    api.listUnattended.mockImplementation((workspaceId: string) =>
      Promise.resolve([
        {
          run_id: `${workspaceId}-run`,
          branch: `eko-unattended-${workspaceId}`,
          path: `/${workspaceId}/worktree`,
          head: 'head',
          status: 'paused',
          active: false,
          locked: false,
          lock_reason: null,
          uncommitted_changes: 0,
          ahead_commits: 0,
          has_changes: false,
          orphan_branch: false,
        },
      ])
    );
    api.cleanupUnattended.mockReturnValue(cleanupA.promise);

    render(<WorktreePanel />);
    const cleanupButtonA = await screen.findByRole('button', { name: /Clean unchanged \(1\)/ });
    fireEvent.click(cleanupButtonA);
    act(() => useWorkspaceStore.setState({ current: workspace('workspace-b') }));

    const cleanupButtonB = await screen.findByRole('button', { name: /Clean unchanged \(1\)/ });
    expect((cleanupButtonB as HTMLButtonElement).disabled).toBe(false);
    await act(async () => {
      cleanupA.resolve({
        removed: [],
        unlocked: [],
        kept: [],
        errors: ['workspace A cleanup warning'],
      });
    });
    expect(screen.queryByText('workspace A cleanup warning')).toBeNull();
  });

  it('drops stale primary and unattended results across A to B to A', async () => {
    const oldPrimary = deferred<Array<Record<string, unknown>>>();
    const oldUnattended = deferred<Array<Record<string, unknown>>>();
    api.list
      .mockReturnValueOnce(oldPrimary.promise)
      .mockResolvedValueOnce([])
      .mockResolvedValueOnce([
        { path: '/a-new', branch: 'a-new', managed: true, head: 'new-head' },
      ]);
    api.listUnattended
      .mockReturnValueOnce(oldUnattended.promise)
      .mockResolvedValueOnce([])
      .mockResolvedValueOnce([]);

    render(<WorktreePanel />);
    act(() => useWorkspaceStore.setState({ current: workspace('workspace-b') }));
    act(() => useWorkspaceStore.setState({ current: workspace('workspace-a') }));
    expect(await screen.findByText('a-new')).toBeTruthy();

    await act(async () => {
      oldPrimary.resolve([{ path: '/a-old', branch: 'a-old', managed: true, head: 'old-head' }]);
      oldUnattended.resolve([
        {
          run_id: 'old-run',
          branch: 'eko-unattended-old',
          path: '/old',
          head: 'old',
          status: 'paused',
          active: false,
          locked: false,
          lock_reason: null,
          uncommitted_changes: 1,
          ahead_commits: 1,
          has_changes: true,
          orphan_branch: false,
        },
      ]);
    });
    expect(screen.queryByText('a-old')).toBeNull();
    expect(screen.queryByText('eko-unattended-old')).toBeNull();
    expect(api.list).toHaveBeenCalledWith('workspace-a');
    expect(api.list).toHaveBeenCalledWith('workspace-b');
  });
});
