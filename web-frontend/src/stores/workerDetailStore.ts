import { create } from 'zustand';

interface WorkerDetailSelection {
  subagentRunId: string;
}

interface WorkerDetailStore {
  selected: WorkerDetailSelection | null;
  selectWorker: (subagentRunId: string) => void;
  close: () => void;
}

export const useWorkerDetailStore = create<WorkerDetailStore>((set) => ({
  selected: null,
  selectWorker: (subagentRunId) => set({ selected: { subagentRunId } }),
  close: () => set({ selected: null }),
}));
