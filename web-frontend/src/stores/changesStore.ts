import { create } from 'zustand';
import type { ChangedFile } from '../utils/deriveChangedFiles';

interface ChangesState {
  /** 由 RightRail 从 messages 派生后注入 */
  files: ChangedFile[];
  /** 当前在抽屉中查看的文件 path,null 则抽屉关闭 */
  selectedPath: string | null;
  /** 上一次检测到的会话 id,用于识别切换 */
  lastActiveId: string | null;

  setFiles: (files: ChangedFile[]) => void;
  setSelected: (path: string | null) => void;
  /** 比较 activeId,若变化则清空 files 与 selectedPath */
  checkSessionChange: (activeId: string | null) => void;
  clear: () => void;
}

export const useChangesStore = create<ChangesState>((set, get) => ({
  files: [],
  selectedPath: null,
  lastActiveId: null,

  setFiles: (files) => set({ files }),

  setSelected: (path) => set({ selectedPath: path }),

  checkSessionChange: (activeId) => {
    if (activeId !== get().lastActiveId) {
      set({
        lastActiveId: activeId,
        files: [],
        selectedPath: null,
      });
    }
  },

  clear: () => set({ files: [], selectedPath: null }),
}));
