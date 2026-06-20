import { create } from 'zustand';

export type WorkerTraceEventKind =
  | 'run_started'
  | 'run_status_changed'
  | 'run_completed'
  | 'run_failed'
  | 'run_cancelled'
  | 'worker_planned'
  | 'worker_started'
  | 'worker_thinking_start'
  | 'worker_thinking_delta'
  | 'worker_thinking_end'
  | 'worker_tool_start'
  | 'worker_tool_result'
  | 'worker_token_delta'
  | 'worker_artifact'
  | 'worker_completed'
  | 'worker_failed'
  | 'worker_cancelled'
  | 'approval_requested'
  | 'approval_resolved'
  | 'note';

export interface WorkerTraceEvent {
  event_id: string;
  run_id: string;
  worker_id?: string | null;
  parent_worker_id?: string | null;
  agent_name?: string | null;
  title?: string | null;
  task?: string | null;
  event_type: WorkerTraceEventKind;
  payload: unknown;
  timestamp: string;
}

export type WorkerTraceStatus = 'planned' | 'running' | 'completed' | 'failed' | 'cancelled';

export interface WorkerTraceState {
  workerId: string;
  runId: string;
  parentWorkerId?: string;
  agentName?: string;
  title?: string;
  task?: string;
  status: WorkerTraceStatus;
  startedAt?: string;
  completedAt?: string;
  events: WorkerTraceEvent[];
}

interface WorkerTraceStore {
  runs: Record<string, WorkerTraceEvent[]>;
  workers: Record<string, WorkerTraceState>;
  append: (event: WorkerTraceEvent) => void;
  clear: () => void;
}

function statusFromEvent(eventType: WorkerTraceEventKind): WorkerTraceStatus | undefined {
  switch (eventType) {
    case 'worker_planned':
      return 'planned';
    case 'worker_started':
    case 'worker_thinking_start':
    case 'worker_thinking_delta':
    case 'worker_thinking_end':
    case 'worker_tool_start':
    case 'worker_tool_result':
    case 'worker_token_delta':
    case 'worker_artifact':
      return 'running';
    case 'worker_completed':
      return 'completed';
    case 'worker_failed':
      return 'failed';
    case 'worker_cancelled':
      return 'cancelled';
    default:
      return undefined;
  }
}

export const useWorkerTraceStore = create<WorkerTraceStore>((set) => ({
  runs: {},
  workers: {},

  append: (event) => {
    set((state) => {
      const runEvents = [...(state.runs[event.run_id] ?? []), event];
      const nextRuns = { ...state.runs, [event.run_id]: runEvents };

      if (!event.worker_id) {
        return { runs: nextRuns };
      }

      const prev = state.workers[event.worker_id];
      const status = statusFromEvent(event.event_type) ?? prev?.status ?? 'running';
      const startedAt =
        prev?.startedAt ?? (event.event_type === 'worker_started' ? event.timestamp : undefined);
      const completedAt =
        status === 'completed' || status === 'failed' || status === 'cancelled'
          ? event.timestamp
          : prev?.completedAt;

      const nextWorker: WorkerTraceState = {
        workerId: event.worker_id,
        runId: event.run_id,
        parentWorkerId: event.parent_worker_id ?? prev?.parentWorkerId,
        agentName: event.agent_name ?? prev?.agentName,
        title: event.title ?? prev?.title,
        task: event.task ?? prev?.task,
        status,
        startedAt,
        completedAt,
        events: [...(prev?.events ?? []), event],
      };

      return {
        runs: nextRuns,
        workers: { ...state.workers, [event.worker_id]: nextWorker },
      };
    });
  },

  clear: () => set({ runs: {}, workers: {} }),
}));
