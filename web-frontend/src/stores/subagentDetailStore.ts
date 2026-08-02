import { create } from 'zustand';

interface SubagentDetailSelection {
  runId: string;
  subagentRunId: string;
}

interface SubagentDetailStore {
  selected: SubagentDetailSelection | null;
  selectSubagent: (runId: string, subagentRunId: string) => void;
  close: () => void;
}

export const useSubagentDetailStore = create<SubagentDetailStore>((set) => ({
  selected: null,
  selectSubagent: (runId, subagentRunId) => set({ selected: { runId, subagentRunId } }),
  close: () => set({ selected: null }),
}));
