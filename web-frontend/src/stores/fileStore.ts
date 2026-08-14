import { create } from 'zustand';
import {
  filesApi,
  type DiffHunk,
  type FileContent,
  type FileEntry,
  type FileTreeNode,
  type WorkspaceChange,
} from '../api/endpoints';
import { errorMessage } from '../lib/tauri-bridge';

export interface FileDocument {
  file: FileContent;
  draft: string;
  dirty: boolean;
  editing: boolean;
  conflict: boolean;
  stale: boolean;
}

interface FileStore {
  tree: FileTreeNode[];
  openFiles: string[];
  selectedFile: string | null;
  documents: Record<string, FileDocument>;
  diffHunks: DiffHunk[];
  changes: WorkspaceChange[];
  loading: boolean;
  saving: boolean;
  error: string | null;
  viewMode: 'content' | 'edit' | 'diff';
  generation: number;
  loadTree: (depth?: number) => Promise<void>;
  loadChanges: () => Promise<void>;
  selectFile: (path: string, forceReload?: boolean) => Promise<void>;
  loadDiff: (path: string, gitRef?: string) => Promise<void>;
  setViewMode: (mode: 'content' | 'edit' | 'diff') => void;
  updateDraft: (content: string) => void;
  saveSelected: () => Promise<boolean>;
  discardSelected: () => Promise<void>;
  refreshSelectedFromDisk: () => Promise<void>;
  closeFile: (path: string, force?: boolean) => boolean;
  clearError: () => void;
  markWorkspaceChanged: () => void;
}

export type { FileEntry, FileContent, FileTreeNode, DiffHunk, WorkspaceChange };

export const useFileStore = create<FileStore>((set, get) => ({
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

  loadTree: async (depth = 4) => {
    set({ loading: true, error: null });
    const generation = get().generation;
    try {
      const tree = await filesApi.tree(depth);
      if (get().generation !== generation) return;
      set({ tree, loading: false });
    } catch (error) {
      if (get().generation !== generation) return;
      set({ error: errorMessage(error), loading: false });
    }
  },

  loadChanges: async () => {
    const generation = get().generation;
    try {
      const changes = await filesApi.changes();
      if (get().generation !== generation) return;
      set({ changes });
    } catch (error) {
      if (get().generation !== generation) return;
      set({ error: errorMessage(error) });
    }
  },

  selectFile: async (path, forceReload = false) => {
    const existing = get().documents[path];
    if (existing && !forceReload && !existing.stale) {
      set({ selectedFile: path, viewMode: existing.editing ? 'edit' : 'content', error: null });
      return;
    }
    if (existing?.dirty && (forceReload || existing.stale)) {
      set((state) => ({
        selectedFile: path,
        documents: {
          ...state.documents,
          [path]: { ...existing, conflict: true },
        },
        error: '文件已在磁盘变化，当前草稿尚未保存',
      }));
      return;
    }

    set({ selectedFile: path, loading: true, error: null, diffHunks: [], viewMode: 'content' });
    const generation = get().generation;
    try {
      const file = await filesApi.read(path);
      if (get().generation !== generation) return;
      set((state) => ({
        openFiles: state.openFiles.includes(path) ? state.openFiles : [...state.openFiles, path],
        documents: {
          ...state.documents,
          [path]: {
            file,
            draft: file.content,
            dirty: false,
            editing: false,
            conflict: false,
            stale: false,
          },
        },
        loading: false,
      }));
    } catch (error) {
      if (get().generation !== generation) return;
      set({ error: errorMessage(error), loading: false });
    }
  },

  loadDiff: async (path, gitRef = 'HEAD') => {
    set({ selectedFile: path, loading: true, error: null, viewMode: 'diff' });
    const generation = get().generation;
    try {
      const [data, file] = await Promise.all([
        filesApi.diff(path, gitRef),
        get().documents[path] ? Promise.resolve(null) : filesApi.read(path).catch(() => null),
      ]);
      if (get().generation !== generation) return;
      set((state) => ({
        openFiles: state.openFiles.includes(path) ? state.openFiles : [...state.openFiles, path],
        documents: file
          ? {
              ...state.documents,
              [path]: {
                file,
                draft: file.content,
                dirty: false,
                editing: false,
                conflict: false,
                stale: false,
              },
            }
          : state.documents,
        diffHunks: data.hunks,
        loading: false,
      }));
    } catch (error) {
      if (get().generation !== generation) return;
      set({ error: errorMessage(error), loading: false });
    }
  },

  setViewMode: (viewMode) => {
    const selectedFile = get().selectedFile;
    if (selectedFile && viewMode === 'edit') {
      const document = get().documents[selectedFile];
      if (!document || document.file.kind !== 'text') return;
      set((state) => ({
        viewMode,
        documents: {
          ...state.documents,
          [selectedFile]: { ...document, editing: true },
        },
      }));
      return;
    }
    set({ viewMode });
  },

  updateDraft: (content) => {
    const selectedFile = get().selectedFile;
    if (!selectedFile) return;
    const document = get().documents[selectedFile];
    if (!document) return;
    set((state) => ({
      documents: {
        ...state.documents,
        [selectedFile]: {
          ...document,
          draft: content,
          dirty: content !== document.file.content,
        },
      },
    }));
  },

  saveSelected: async () => {
    const selectedFile = get().selectedFile;
    if (!selectedFile) return false;
    const document = get().documents[selectedFile];
    if (!document || !document.dirty || document.file.kind !== 'text') return true;
    if (document.stale) {
      set({ error: '工作区已变化，请先恢复当前工作区的磁盘版本', saving: false });
      return false;
    }
    set({ saving: true, error: null });
    const generation = get().generation;
    try {
      const file = await filesApi.write(
        selectedFile,
        document.draft,
        document.file.workspace_id,
        document.file.revision
      );
      if (get().generation !== generation) return false;
      set((state) => {
        const current = state.documents[selectedFile];
        if (!current) return { saving: false };
        const editedDuringSave = current.draft !== document.draft;
        return {
          saving: false,
          documents: {
            ...state.documents,
            [selectedFile]: {
              file,
              draft: editedDuringSave ? current.draft : file.content,
              dirty: editedDuringSave,
              editing: true,
              conflict: false,
              stale: false,
            },
          },
        };
      });
      void get().loadChanges();
      return true;
    } catch (error) {
      const message = errorMessage(error);
      set((state) => ({
        saving: false,
        error: message,
        documents: {
          ...state.documents,
          [selectedFile]: {
            ...(state.documents[selectedFile] ?? document),
            conflict: message.includes('changed on disk') || message.includes('Workspace changed'),
          },
        },
      }));
      return false;
    }
  },

  discardSelected: async () => {
    const selectedFile = get().selectedFile;
    if (!selectedFile) return;
    set((state) => {
      const documents = { ...state.documents };
      delete documents[selectedFile];
      return { documents };
    });
    await get().selectFile(selectedFile, true);
  },

  refreshSelectedFromDisk: async () => {
    const selectedFile = get().selectedFile;
    if (!selectedFile) return;
    const document = get().documents[selectedFile];
    if (!document) return;
    const generation = get().generation;
    try {
      const file = await filesApi.read(selectedFile);
      if (get().generation !== generation) return;
      const latest = get().documents[selectedFile];
      if (
        !latest ||
        latest.file.revision !== document.file.revision ||
        latest.file.workspace_id !== document.file.workspace_id ||
        file.workspace_id !== document.file.workspace_id
      )
        return;
      if (file.revision === latest.file.revision) return;
      if (latest.dirty) {
        set((state) => {
          const current = state.documents[selectedFile];
          if (!current || current.file.revision !== document.file.revision) return state;
          return {
            documents: {
              ...state.documents,
              [selectedFile]: { ...current, conflict: true },
            },
            error: '文件已被 Agent 或外部程序修改，当前草稿未覆盖磁盘内容',
          };
        });
        return;
      }
      set((state) => {
        const current = state.documents[selectedFile];
        if (!current || current.file.revision !== document.file.revision) return state;
        if (current.dirty) {
          return {
            documents: {
              ...state.documents,
              [selectedFile]: { ...current, conflict: true },
            },
            error: '文件已被 Agent 或外部程序修改，当前草稿未覆盖磁盘内容',
          };
        }
        return {
          documents: {
            ...state.documents,
            [selectedFile]: {
              file,
              draft: file.content,
              dirty: false,
              editing: current.editing,
              conflict: false,
              stale: false,
            },
          },
        };
      });
    } catch (error) {
      if (get().generation !== generation) return;
      set({ error: errorMessage(error) });
    }
  },

  closeFile: (path, force = false) => {
    const state = get();
    if (state.documents[path]?.dirty && !force) return false;
    const openFiles = state.openFiles.filter((item) => item !== path);
    const documents = { ...state.documents };
    delete documents[path];
    const selectedFile =
      state.selectedFile === path ? (openFiles.at(-1) ?? null) : state.selectedFile;
    set({ openFiles, documents, selectedFile, diffHunks: [], viewMode: 'content' });
    return true;
  },

  clearError: () => set({ error: null }),

  markWorkspaceChanged: () =>
    set((state) => ({
      generation: state.generation + 1,
      tree: [],
      changes: [],
      diffHunks: [],
      loading: false,
      saving: false,
      documents: Object.fromEntries(
        Object.entries(state.documents).map(([path, document]) => [
          path,
          { ...document, stale: true, conflict: true },
        ])
      ),
      error:
        Object.keys(state.documents).length > 0 ? '工作区已变化，已打开的文件需要重新加载' : null,
    })),
}));
