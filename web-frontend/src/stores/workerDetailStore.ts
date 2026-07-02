import { create } from 'zustand';

interface WorkerDetailSelection {
  runId: string;
  workerId: string;
}

interface WorkerDetailStore {
  selected: WorkerDetailSelection | null;
  selectWorker: (runId: string, workerId: string) => void;
  close: () => void;
}

export const useWorkerDetailStore = create<WorkerDetailStore>((set) => ({
  selected: null,
  selectWorker: (runId, workerId) => set({ selected: { runId, workerId } }),
  close: () => set({ selected: null }),
}));
