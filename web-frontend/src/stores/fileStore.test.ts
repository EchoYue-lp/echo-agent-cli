import { beforeEach, describe, expect, it, vi } from 'vitest';

const mocks = vi.hoisted(() => ({
  tree: vi.fn(),
  changes: vi.fn(),
  read: vi.fn(),
  write: vi.fn(),
  diff: vi.fn(),
}));

vi.mock('../api/endpoints', () => ({
  filesApi: mocks,
}));

import { useFileStore } from './fileStore';

const initialFile = {
  workspace_id: 'workspace:a',
  workspace_generation: 'generation-a',
  path: 'src/main.ts',
  content: 'const value = 1;\n',
  size: 17,
  language: 'typescript',
  kind: 'text' as const,
  mime_type: 'text/plain',
  data_url: null,
  revision: 'revision-1',
};

describe('fileStore', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useFileStore.setState({
      scope: { workspaceId: 'workspace:a', workspaceGeneration: 'generation-a' },
      tree: [],
      openFiles: [],
      selectedFile: null,
      documents: {},
      diffHunks: [],
      changes: [],
      loading: false,
      saving: false,
      error: null,
      viewMode: 'content',
      generation: 0,
    });
    mocks.read.mockResolvedValue(initialFile);
    mocks.tree.mockResolvedValue([]);
    mocks.changes.mockResolvedValue([]);
    mocks.diff.mockResolvedValue({ hunks: [] });
  });

  it('loads through filesApi and tracks a dirty editable draft', async () => {
    await useFileStore.getState().selectFile(initialFile.path);
    useFileStore.getState().setViewMode('edit');
    useFileStore.getState().updateDraft('const value = 2;\n');

    expect(mocks.read).toHaveBeenCalledWith(
      { workspaceId: 'workspace:a', workspaceGeneration: 'generation-a' },
      initialFile.path
    );
    expect(useFileStore.getState().documents[initialFile.path]?.dirty).toBe(true);
  });

  it('saves with the expected revision and adopts the returned revision', async () => {
    mocks.write.mockResolvedValue({
      ...initialFile,
      content: 'const value = 2;\n',
      revision: 'revision-2',
    });
    await useFileStore.getState().selectFile(initialFile.path);
    useFileStore.getState().setViewMode('edit');
    useFileStore.getState().updateDraft('const value = 2;\n');

    expect(await useFileStore.getState().saveSelected()).toBe(true);
    expect(mocks.write).toHaveBeenCalledWith(
      { workspaceId: 'workspace:a', workspaceGeneration: 'generation-a' },
      initialFile.path,
      'const value = 2;\n',
      'revision-1'
    );
    expect(useFileStore.getState().documents[initialFile.path]?.file.revision).toBe('revision-2');
  });

  it('keeps the draft and marks a conflict when the disk revision changed', async () => {
    mocks.write.mockRejectedValue(new Error('File changed on disk; reload it before saving'));
    await useFileStore.getState().selectFile(initialFile.path);
    useFileStore.getState().setViewMode('edit');
    useFileStore.getState().updateDraft('const value = 3;\n');

    expect(await useFileStore.getState().saveSelected()).toBe(false);
    const document = useFileStore.getState().documents[initialFile.path];
    expect(document?.draft).toBe('const value = 3;\n');
    expect(document?.conflict).toBe(true);
  });

  it('detects an external edit without overwriting a dirty draft', async () => {
    await useFileStore.getState().selectFile(initialFile.path);
    useFileStore.getState().setViewMode('edit');
    useFileStore.getState().updateDraft('local draft\n');
    mocks.read.mockResolvedValue({
      ...initialFile,
      content: 'agent edit\n',
      revision: 'revision-agent',
    });

    await useFileStore.getState().refreshSelectedFromDisk();
    const document = useFileStore.getState().documents[initialFile.path];
    expect(document?.draft).toBe('local draft\n');
    expect(document?.conflict).toBe(true);
  });

  it('preserves typing that happens while a save is in flight', async () => {
    let resolveWrite: ((value: typeof initialFile) => void) | undefined;
    mocks.write.mockReturnValue(
      new Promise<typeof initialFile>((resolve) => {
        resolveWrite = resolve;
      })
    );
    await useFileStore.getState().selectFile(initialFile.path);
    useFileStore.getState().setViewMode('edit');
    useFileStore.getState().updateDraft('submitted draft\n');

    const save = useFileStore.getState().saveSelected();
    useFileStore.getState().updateDraft('new typing\n');
    resolveWrite?.({ ...initialFile, content: 'submitted draft\n', revision: 'revision-2' });

    expect(await save).toBe(true);
    const document = useFileStore.getState().documents[initialFile.path];
    expect(document?.draft).toBe('new typing\n');
    expect(document?.dirty).toBe(true);
    expect(document?.file.revision).toBe('revision-2');
  });

  it('preserves typing that happens while a disk refresh is in flight', async () => {
    await useFileStore.getState().selectFile(initialFile.path);
    useFileStore.getState().setViewMode('edit');
    let resolveRead: ((value: typeof initialFile) => void) | undefined;
    mocks.read.mockReturnValue(
      new Promise<typeof initialFile>((resolve) => {
        resolveRead = resolve;
      })
    );

    const refresh = useFileStore.getState().refreshSelectedFromDisk();
    useFileStore.getState().updateDraft('typed during refresh\n');
    resolveRead?.({ ...initialFile, content: 'agent edit\n', revision: 'revision-agent' });

    await refresh;
    const document = useFileStore.getState().documents[initialFile.path];
    expect(document?.draft).toBe('typed during refresh\n');
    expect(document?.conflict).toBe(true);
  });

  it('keeps a dirty draft stale and refuses to save after a workspace switch', async () => {
    await useFileStore.getState().selectFile(initialFile.path);
    useFileStore.getState().setViewMode('edit');
    useFileStore.getState().updateDraft('workspace A draft\n');

    useFileStore
      .getState()
      .bindScope({ workspaceId: 'workspace:b', workspaceGeneration: 'generation-b' });

    expect(await useFileStore.getState().saveSelected()).toBe(false);
    const document = useFileStore.getState().documents[initialFile.path];
    expect(document?.draft).toBe('workspace A draft\n');
    expect(document?.stale).toBe(true);
    expect(mocks.write).not.toHaveBeenCalled();
  });

  it('drops a late read result from the previous workspace generation', async () => {
    let resolveRead: ((value: typeof initialFile) => void) | undefined;
    mocks.read.mockReturnValue(
      new Promise<typeof initialFile>((resolve) => {
        resolveRead = resolve;
      })
    );

    const select = useFileStore.getState().selectFile(initialFile.path);
    useFileStore.getState().markWorkspaceChanged();
    resolveRead?.(initialFile);
    await select;

    expect(useFileStore.getState().documents[initialFile.path]).toBeUndefined();
  });

  it('reloads the current workspace version after explicitly discarding a stale draft', async () => {
    await useFileStore.getState().selectFile(initialFile.path);
    useFileStore.getState().setViewMode('edit');
    useFileStore.getState().updateDraft('workspace A draft\n');
    useFileStore
      .getState()
      .bindScope({ workspaceId: 'workspace:b', workspaceGeneration: 'generation-b' });
    mocks.read.mockResolvedValue({
      ...initialFile,
      workspace_id: 'workspace:b',
      workspace_generation: 'generation-b',
      content: 'workspace B content\n',
      revision: 'revision-b',
    });

    await useFileStore.getState().discardSelected();

    const document = useFileStore.getState().documents[initialFile.path];
    expect(document?.file.workspace_id).toBe('workspace:b');
    expect(document?.draft).toBe('workspace B content\n');
    expect(document?.stale).toBe(false);
  });

  it('keeps a draft stale when its save completes after switching workspaces', async () => {
    let resolveWrite: ((value: typeof initialFile) => void) | undefined;
    mocks.write.mockReturnValue(
      new Promise<typeof initialFile>((resolve) => {
        resolveWrite = resolve;
      })
    );
    await useFileStore.getState().selectFile(initialFile.path);
    useFileStore.getState().setViewMode('edit');
    useFileStore.getState().updateDraft('workspace A draft\n');

    const save = useFileStore.getState().saveSelected();
    useFileStore
      .getState()
      .bindScope({ workspaceId: 'workspace:b', workspaceGeneration: 'generation-b' });
    resolveWrite?.({ ...initialFile, content: 'workspace A draft\n', revision: 'revision-2' });

    expect(await save).toBe(false);
    const document = useFileStore.getState().documents[initialFile.path];
    expect(document?.stale).toBe(true);
    expect(document?.draft).toBe('workspace A draft\n');
  });

  it('ignores a late save error from the previous workspace generation', async () => {
    let rejectWrite: ((reason: Error) => void) | undefined;
    mocks.write.mockReturnValue(
      new Promise<typeof initialFile>((_resolve, reject) => {
        rejectWrite = reject;
      })
    );
    await useFileStore.getState().selectFile(initialFile.path);
    useFileStore.getState().setViewMode('edit');
    useFileStore.getState().updateDraft('workspace A draft\n');

    const save = useFileStore.getState().saveSelected();
    useFileStore
      .getState()
      .bindScope({ workspaceId: 'workspace:b', workspaceGeneration: 'generation-b' });
    const workspaceError = useFileStore.getState().error;
    rejectWrite?.(new Error('old workspace save failed'));

    expect(await save).toBe(false);
    expect(useFileStore.getState().error).toBe(workspaceError);
  });
});
