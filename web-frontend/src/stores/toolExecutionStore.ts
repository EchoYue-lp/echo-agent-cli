import { create } from 'zustand';
import type { ToolExecution, ToolExecutionOwner } from '../types/api';

interface ToolExecutionState {
  tools: Record<string, ToolExecution>;
  idsByOwner: Record<string, string[]>;
  ingest: (tool: ToolExecution) => void;
  replaceAll: (tools: ToolExecution[]) => void;
  clear: () => void;
}

export function toolExecutionOwnerKey(owner: ToolExecutionOwner): string {
  return owner.kind === 'chat' ? `chat:${owner.message_id}` : `subagent:${owner.subagent_run_id}`;
}

export const useToolExecutionStore = create<ToolExecutionState>((set) => ({
  tools: {},
  idsByOwner: {},

  ingest: (tool) => {
    set((state) => {
      const ownerKey = toolExecutionOwnerKey(tool.owner);
      const ownerIds = state.idsByOwner[ownerKey] ?? [];
      return {
        tools: { ...state.tools, [tool.id]: tool },
        idsByOwner: ownerIds.includes(tool.id)
          ? state.idsByOwner
          : { ...state.idsByOwner, [ownerKey]: [...ownerIds, tool.id] },
      };
    });
  },

  replaceAll: (tools) => {
    set(() => {
      const nextTools: Record<string, ToolExecution> = {};
      const nextIdsByOwner: Record<string, string[]> = {};
      for (const tool of tools) {
        nextTools[tool.id] = tool;
        const ownerKey = toolExecutionOwnerKey(tool.owner);
        const ownerIds = nextIdsByOwner[ownerKey] ?? [];
        if (!ownerIds.includes(tool.id)) nextIdsByOwner[ownerKey] = [...ownerIds, tool.id];
      }
      return { tools: nextTools, idsByOwner: nextIdsByOwner };
    });
  },

  clear: () => set({ tools: {}, idsByOwner: {} }),
}));
