import { create } from 'zustand';
import { get } from '../api/client';

interface FileEntry {
  name: string;
  path: string;
  is_dir: boolean;
  size: number;
  modified?: string;
  extension?: string;
}

interface TreeNode {
  name: string;
  path: string;
  is_dir: boolean;
  children?: TreeNode[];
}

interface FileContent {
  path: string;
  content: string;
  size: number;
  language?: string;
}

interface DiffHunk {
  old_start: number;
  old_count: number;
  new_start: number;
  new_count: number;
  lines: { tag: string; old_line?: number; new_line?: number; content: string }[];
}

interface FileStore {
  tree: TreeNode[];
  selectedFile: string | null;
  fileContent: FileContent | null;
  diffHunks: DiffHunk[];
  loading: boolean;
  error: string | null;
  viewMode: 'content' | 'diff';

  loadTree: (depth?: number) => Promise<void>;
  selectFile: (path: string) => Promise<void>;
  loadDiff: (path: string, gitRef?: string) => Promise<void>;
  clearSelection: () => void;
  setViewMode: (mode: 'content' | 'diff') => void;
}

export type { FileEntry, TreeNode, FileContent, DiffHunk };

export const useFileStore = create<FileStore>((set) => ({
  tree: [],
  selectedFile: null,
  fileContent: null,
  diffHunks: [],
  loading: false,
  error: null,
  viewMode: 'content',

  loadTree: async (depth = 3) => {
    set({ loading: true, error: null });
    try {
      const data = await get<TreeNode[]>(`/files/tree?depth=${depth}`);
      set({ tree: data, loading: false });
    } catch (e: unknown) {
      set({ error: e instanceof Error ? e.message : String(e), loading: false });
    }
  },

  selectFile: async (path: string) => {
    set({ selectedFile: path, loading: true, error: null, diffHunks: [], viewMode: 'content' });
    try {
      const data = await get<FileContent>(`/files/read?path=${encodeURIComponent(path)}`);
      set({ fileContent: data, loading: false });
    } catch (e: unknown) {
      set({ error: e instanceof Error ? e.message : String(e), loading: false });
    }
  },

  loadDiff: async (path: string, gitRef = 'HEAD') => {
    set({ selectedFile: path, loading: true, error: null, viewMode: 'diff' });
    try {
      const data = await get<{
        path: string;
        old_content: string;
        new_content: string;
        hunks: DiffHunk[];
      }>(`/files/diff?path=${encodeURIComponent(path)}&git_ref=${gitRef}`);
      set({ diffHunks: data.hunks, fileContent: null, loading: false });
    } catch (e: unknown) {
      set({ error: e instanceof Error ? e.message : String(e), loading: false });
    }
  },

  clearSelection: () =>
    set({ selectedFile: null, fileContent: null, diffHunks: [], viewMode: 'content' }),

  setViewMode: (mode) => set({ viewMode: mode }),
}));
