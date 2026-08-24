// @vitest-environment jsdom
import { render } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';

const mocks = vi.hoisted(() => ({
  workspaceState: {
    workspaces: [
      {
        id: 'workspace-1',
        name: '研究工作区',
        root: '/tmp/research',
        kind: { type: 'research' },
        metadata: { tags: [] },
        product_data_generation: 'workspace-generation',
        created_at: '2026-08-14T00:00:00Z',
        last_active: '2026-08-14T00:00:00Z',
      },
    ],
    current: null,
    init: vi.fn(),
    switchTo: vi.fn(),
    createAndSwitch: vi.fn(),
    delete: vi.fn(),
  },
}));

vi.mock('../../stores/workspaceStore', () => ({
  useWorkspaceStore: (selector: (state: typeof mocks.workspaceState) => unknown) =>
    selector(mocks.workspaceState),
}));

vi.mock('../../api/endpoints', () => ({
  workspaceApi: {
    defaultRoot: vi.fn(),
  },
}));

vi.mock('../../lib/tauri-bridge', () => ({
  fileSystem: { selectDirectory: vi.fn() },
  isTauri: () => false,
}));

vi.mock('../../api/client', () => ({
  get: vi.fn().mockResolvedValue({
    current: '/tmp',
    parent: '/',
    entries: [],
  }),
}));

import DirectoryPicker from './DirectoryPicker';
import NewTaskDialog from './NewTaskDialog';

describe('workspace dialog accessibility', () => {
  it('gives the New Task close and workspace actions distinct button semantics', () => {
    const { getByRole } = render(<NewTaskDialog isOpen onClose={vi.fn()} />);

    expect(getByRole('button', { name: '关闭任务工作区' })).toBeTruthy();
    const selectWorkspace = getByRole('button', {
      name: '研究工作区 research · /tmp/research',
    });
    const deleteWorkspace = getByRole('button', { name: '删除工作区 研究工作区' });
    expect(selectWorkspace).not.toBe(deleteWorkspace);
    expect(selectWorkspace.contains(deleteWorkspace)).toBe(false);
  });

  it('exposes an accessible name for the Directory Picker close control', () => {
    const { getByRole } = render(<DirectoryPicker isOpen onClose={vi.fn()} onSelect={vi.fn()} />);

    expect(getByRole('button', { name: '关闭文件夹选择器' })).toBeTruthy();
  });
});
