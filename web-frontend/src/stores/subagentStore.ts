import { create } from 'zustand';

export type SubagentStatus = 'running' | 'completed' | 'failed' | 'cancelled';

export interface SubagentState {
  id: string;
  agent: string;
  parent: string;
  task: string;
  mode: string;
  status: SubagentStatus;
  startedAt: number;
  durationMs?: number;
  error?: string;
  tokensUsed?: number;
  iterationCount?: number;
}

interface SubagentStore {
  subagents: Record<string, SubagentState>;
  upsert: (ev: SubagentEventPayload) => void;
  gc: () => void;
  clear: () => void;
}

export interface SubagentEventPayload {
  type: 'subagent_started' | 'subagent_completed' | 'subagent_failed' | 'subagent_cancelled';
  parent: string;
  agent: string;
  dispatch_id?: string;
  mode?: string;
  task?: string;
  duration_ms?: number;
  error?: string;
  tokens_used?: number;
  iteration_count?: number;
}

export const useSubagentStore = create<SubagentStore>((set) => ({
  subagents: {},

  upsert: (ev) => {
    set((s) => {
      const sameDispatch = Object.entries(s.subagents).filter(([, subagent]) =>
        ev.dispatch_id
          ? subagent.id === ev.dispatch_id
          : subagent.parent === ev.parent && subagent.agent === ev.agent
      );
      const runningDispatch = sameDispatch
        .filter(([, subagent]) => subagent.status === 'running')
        .sort(([, a], [, b]) => a.startedAt - b.startedAt)[0];
      const key =
        ev.dispatch_id ??
        (ev.type === 'subagent_started'
          ? `${ev.parent}:${ev.agent}:${Date.now()}:${Math.random().toString(36).slice(2)}`
          : (runningDispatch?.[0] ??
            sameDispatch[sameDispatch.length - 1]?.[0] ??
            `${ev.parent}:${ev.agent}`));
      const prev = s.subagents[key];
      const next: SubagentState = {
        id: prev?.id ?? key,
        agent: ev.agent,
        parent: ev.parent,
        task: ev.task ?? prev?.task ?? '',
        mode: ev.mode ?? prev?.mode ?? 'sync',
        status:
          ev.type === 'subagent_started'
            ? 'running'
            : ev.type === 'subagent_completed'
              ? 'completed'
              : ev.type === 'subagent_failed'
                ? 'failed'
                : 'cancelled',
        startedAt: prev?.startedAt ?? Date.now(),
        durationMs: ev.duration_ms ?? prev?.durationMs,
        error: ev.error ?? prev?.error,
        tokensUsed: ev.tokens_used ?? prev?.tokensUsed,
        iterationCount: ev.iteration_count ?? prev?.iterationCount,
      };
      return { subagents: { ...s.subagents, [key]: next } };
    });
  },

  // GC: remove completed/failed entries older than 5 minutes to prevent
  // unbounded memory growth in long-running sessions (P2).
  gc: () => {
    const cutoff = Date.now() - 5 * 60 * 1000;
    set((s) => {
      const filtered: Record<string, SubagentState> = {};
      for (const [k, v] of Object.entries(s.subagents)) {
        if (v.status === 'running' || v.startedAt > cutoff) {
          filtered[k] = v;
        }
      }
      return { subagents: filtered };
    });
  },

  clear: () => set({ subagents: {} }),
}));

// Run GC every 60 seconds when there are entries to clean
if (typeof window !== 'undefined') {
  setInterval(() => {
    const store = useSubagentStore.getState();
    if (Object.keys(store.subagents).length > 0) {
      store.gc();
    }
  }, 60_000);
}
