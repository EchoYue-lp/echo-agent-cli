import { create } from 'zustand';

export type ConversationEventKind =
  | 'route_decision'
  | 'initial_thinking'
  | 'worker_started'
  | 'worker_tool_call'
  | 'worker_result'
  | 'llm_usage'
  | 'final_answer'
  | 'approval_request'
  | 'error';

export interface ConversationRuntimeEventData {
  type: ConversationEventKind;
  route?: string;
  confidence?: number;
  reason?: string;
  matched_feedback_pattern?: string | null;
  suggested_workers?: string[];
  interaction_mode?: string;
  worker_id?: string | null;
  agent_role?: string;
  title?: string;
  task_description?: string;
  tool_name?: string;
  tool_args?: unknown;
  success?: boolean;
  summary?: string;
  files_changed?: string[];
  model?: string;
  input_tokens?: number;
  output_tokens?: number;
  cached_input_tokens?: number;
  cache_creation_input_tokens?: number;
  usage_reported?: boolean;
  content?: string;
  usage_summary?: unknown;
  request_id?: string;
  prompt?: string;
  stage?: string;
  message?: string;
}

export interface WorkerState {
  workerId: string;
  agentRole: string;
  title: string;
  status: 'running' | 'completed' | 'failed';
  summary?: string;
  filesChanged: string[];
}

interface ConversationRuntimeState {
  events: ConversationRuntimeEventData[];
  workers: Map<string, WorkerState>;
  activeRunId: string | null;

  appendEvent: (event: ConversationRuntimeEventData) => void;
  replayFromEvents: (events: ConversationRuntimeEventData[]) => void;
  clear: () => void;
}

export const useConversationRuntimeStore = create<ConversationRuntimeState>((set) => ({
  events: [],
  workers: new Map(),
  activeRunId: null,

  appendEvent: (event) => {
    set((state) => {
      const events = [...state.events, event];
      const workers = new Map(state.workers);

      // Update worker state based on event type
      if (event.type === 'worker_started' && event.worker_id) {
        workers.set(event.worker_id, {
          workerId: event.worker_id,
          agentRole: event.agent_role || 'unknown',
          title: event.title || event.worker_id,
          status: 'running',
          filesChanged: [],
        });
      } else if (event.type === 'worker_result' && event.worker_id) {
        const existing = workers.get(event.worker_id);
        if (existing) {
          workers.set(event.worker_id, {
            ...existing,
            status: 'completed',
            summary: event.summary,
            filesChanged: event.files_changed || [],
          });
        }
      } else if (event.type === 'error' && event.worker_id) {
        const existing = workers.get(event.worker_id);
        if (existing) {
          workers.set(event.worker_id, { ...existing, status: 'failed' });
        }
      }

      return { events, workers };
    });
  },

  replayFromEvents: (events) => {
    const workers = new Map<string, WorkerState>();
    for (const event of events) {
      if (event.type === 'worker_started' && event.worker_id) {
        workers.set(event.worker_id, {
          workerId: event.worker_id,
          agentRole: event.agent_role || 'unknown',
          title: event.title || event.worker_id,
          status: 'running',
          filesChanged: [],
        });
      } else if (event.type === 'worker_result' && event.worker_id) {
        const existing = workers.get(event.worker_id);
        if (existing) {
          workers.set(event.worker_id, {
            ...existing,
            status: 'completed',
            summary: event.summary,
            filesChanged: event.files_changed || [],
          });
        }
      }
    }
    set({ events, workers });
  },

  clear: () => set({ events: [], workers: new Map(), activeRunId: null }),
}));
