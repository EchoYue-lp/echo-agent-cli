import { create } from 'zustand';

interface SubagentDetailSelection {
  subagentRunId: string;
}

interface SubagentDetailStore {
  selected: SubagentDetailSelection | null;
  selectSubagent: (subagentRunId: string) => void;
  close: () => void;
}

export const useSubagentDetailStore = create<SubagentDetailStore>((set) => ({
  selected: null,
  selectSubagent: (subagentRunId) => set({ selected: { subagentRunId } }),
  close: () => set({ selected: null }),
}));
