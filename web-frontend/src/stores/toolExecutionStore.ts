import { create } from 'zustand';
import type { RuntimeTaskEvent, TaskRun } from '../generated';
import type { ToolExecution, ToolExecutionOwner } from '../types/api';

interface ToolExecutionState {
  tools: Record<string, ToolExecution>;
  idsByOwner: Record<string, string[]>;
  ingest: (tool: ToolExecution) => void;
  replaceAll: (tools: ToolExecution[]) => void;
  hydrateConversation: (conversationId: string, tools: ToolExecution[]) => void;
  clear: () => void;
}

const EMPTY_TOOL_EXECUTION_IDS: readonly string[] = [];

export function toolExecutionOwnerKey(owner: ToolExecutionOwner, runId?: string | null): string {
  return owner.kind === 'chat'
    ? `chat:${owner.message_id}`
    : `subagent:${runId ?? ''}:${owner.subagent_run_id}`;
}

export function toolExecutionIdsForOwner(
  idsByOwner: Record<string, string[]>,
  ownerKey: string
): readonly string[] {
  return idsByOwner[ownerKey] ?? EMPTY_TOOL_EXECUTION_IDS;
}

type JsonRecord = Record<string, unknown>;

function jsonRecord(value: unknown): JsonRecord | null {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
    ? (value as JsonRecord)
    : null;
}

function jsonString(value: unknown): string | undefined {
  return typeof value === 'string' ? value : undefined;
}

function eventTimestamp(timestamp: string): number {
  const parsed = Date.parse(timestamp);
  return Number.isFinite(parsed) ? parsed : Date.now();
}

function executionIdentity(tool: Pick<ToolExecution, 'owner' | 'call_id' | 'run_id'>): string {
  return `${toolExecutionOwnerKey(tool.owner, tool.run_id)}\u0000${tool.call_id}`;
}

function toolStatusRank(tool: ToolExecution): number {
  return tool.status === 'running' ? 0 : 1;
}

function toolActivityTimestamp(tool: ToolExecution): number {
  return Math.max(tool.started_at, tool.finished_at ?? tool.started_at);
}

function mergeToolExecution(current: ToolExecution, incoming: ToolExecution): ToolExecution {
  const currentRank = toolStatusRank(current);
  const incomingRank = toolStatusRank(incoming);
  const preferred =
    currentRank !== incomingRank
      ? currentRank > incomingRank
        ? current
        : incoming
      : toolActivityTimestamp(current) >= toolActivityTimestamp(incoming)
        ? current
        : incoming;
  const supplemental = preferred === current ? incoming : current;
  const startedAt = Math.min(current.started_at, incoming.started_at);
  const finishedAt = preferred.status === 'running' ? null : preferred.finished_at;
  return {
    ...supplemental,
    ...preferred,
    id: preferred.detail_ref
      ? preferred.id
      : supplemental.detail_ref
        ? supplemental.id
        : preferred.id,
    args_preview: preferred.args_preview || supplemental.args_preview,
    detail_ref: preferred.detail_ref || supplemental.detail_ref,
    started_at: startedAt,
    finished_at: finishedAt,
    duration_ms: finishedAt == null ? null : Math.max(0, finishedAt - startedAt),
  };
}

function mergeTaskRuntimeBoundary(
  persisted: ToolExecution,
  runtimeBoundary: ToolExecution
): ToolExecution {
  if (runtimeBoundary.status === 'running' || persisted.status === runtimeBoundary.status) {
    return persisted;
  }
  const startedAt = Math.min(persisted.started_at, runtimeBoundary.started_at);
  const finishedAt = runtimeBoundary.finished_at;
  return {
    ...persisted,
    status: runtimeBoundary.status,
    started_at: startedAt,
    finished_at: finishedAt,
    duration_ms: finishedAt == null ? null : Math.max(0, finishedAt - startedAt),
  };
}

export function mergeHydratedToolExecutions(
  current: readonly ToolExecution[],
  incoming: readonly ToolExecution[]
): ToolExecution[] {
  const merged = new Map<string, ToolExecution>();
  for (const tool of [...current, ...incoming]) {
    const identity = executionIdentity(tool);
    const existing = merged.get(identity);
    merged.set(identity, existing ? mergeToolExecution(existing, tool) : tool);
  }
  return [...merged.values()];
}

function indexTools(tools: readonly ToolExecution[]): {
  tools: Record<string, ToolExecution>;
  idsByOwner: Record<string, string[]>;
} {
  const nextTools: Record<string, ToolExecution> = {};
  const nextIdsByOwner: Record<string, string[]> = {};
  for (const tool of tools) {
    nextTools[tool.id] = tool;
    const ownerKey = toolExecutionOwnerKey(tool.owner, tool.run_id);
    const ownerIds = nextIdsByOwner[ownerKey] ?? [];
    if (!ownerIds.includes(tool.id)) nextIdsByOwner[ownerKey] = [...ownerIds, tool.id];
  }
  return { tools: nextTools, idsByOwner: nextIdsByOwner };
}

/** Recover tool rows for TaskRuntime paths without detailed tool persistence. */
export function taskRuntimeToolExecutions(
  run: TaskRun,
  events: readonly RuntimeTaskEvent[]
): ToolExecution[] {
  const tools = new Map<string, ToolExecution>();

  for (const event of events) {
    if (
      event.run_id !== run.run_id ||
      (event.event_type !== 'tool_started' &&
        event.event_type !== 'tool_completed' &&
        event.event_type !== 'tool_failed')
    ) {
      continue;
    }
    const payload = jsonRecord(event.payload);
    const subagentRunId = jsonString(payload?.execution_id);
    const callId = jsonString(payload?.call_id) ?? event.step_id ?? undefined;
    const name = jsonString(payload?.tool_name);
    if (!subagentRunId || !callId || !name) continue;

    const id = `runtime-tool:${run.run_id}:${subagentRunId}:${callId}`;
    const previous = tools.get(id);
    const timestamp = eventTimestamp(event.timestamp);
    const terminal = event.event_type !== 'tool_started';
    const startedAt = previous?.started_at ?? timestamp;
    tools.set(id, {
      id,
      call_id: callId,
      owner: { kind: 'subagent', subagent_run_id: subagentRunId },
      conversation_id: run.conversation_id,
      run_id: run.run_id,
      name,
      args_preview: previous?.args_preview ?? '',
      status:
        event.event_type === 'tool_failed'
          ? 'failed'
          : event.event_type === 'tool_completed'
            ? 'succeeded'
            : 'running',
      started_at: startedAt,
      finished_at: terminal ? timestamp : null,
      duration_ms: terminal ? Math.max(0, timestamp - startedAt) : null,
      // Empty means only the runtime boundary is available. InlineToolCall
      // keeps the row visible without requesting a nonexistent detail file.
      detail_ref: '',
    });
  }

  return [...tools.values()];
}

/** Keep full persisted details while treating TaskRuntime terminal facts as authoritative. */
export function mergeTaskRuntimeToolExecutions(
  persisted: readonly ToolExecution[],
  fallback: readonly ToolExecution[]
): ToolExecution[] {
  const merged = new Map<string, ToolExecution>();
  for (const tool of persisted) merged.set(executionIdentity(tool), tool);
  for (const boundary of fallback) {
    const identity = executionIdentity(boundary);
    const existing = merged.get(identity);
    merged.set(identity, existing ? mergeTaskRuntimeBoundary(existing, boundary) : boundary);
  }
  return [...merged.values()];
}

export const useToolExecutionStore = create<ToolExecutionState>((set) => ({
  tools: {},
  idsByOwner: {},

  ingest: (tool) => {
    set((state) => {
      const ownerKey = toolExecutionOwnerKey(tool.owner, tool.run_id);
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
    set(() => indexTools(tools));
  },

  hydrateConversation: (conversationId, tools) => {
    set((state) => {
      const currentTools = Object.values(state.tools);
      const otherConversationTools = currentTools.filter(
        (tool) => tool.conversation_id !== conversationId
      );
      const liveTools = currentTools.filter((tool) => tool.conversation_id === conversationId);
      return indexTools([
        ...otherConversationTools,
        ...mergeHydratedToolExecutions(liveTools, tools),
      ]);
    });
  },

  clear: () => set({ tools: {}, idsByOwner: {} }),
}));

/** Add runtime-only rows incrementally without duplicating full summaries. */
export function ingestTaskRuntimeToolExecutions(
  run: TaskRun,
  events: readonly RuntimeTaskEvent[]
): void {
  for (const projected of taskRuntimeToolExecutions(run, events)) {
    const state = useToolExecutionStore.getState();
    const existing = Object.values(state.tools).find(
      (tool) => executionIdentity(tool) === executionIdentity(projected)
    );
    useToolExecutionStore
      .getState()
      .ingest(existing ? mergeTaskRuntimeBoundary(existing, projected) : projected);
  }
}
