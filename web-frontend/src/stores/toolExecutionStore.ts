import { create } from 'zustand';
import type { RuntimeTaskEvent, TaskRun } from '../generated';
import type { ToolExecution, ToolExecutionOwner } from '../types/api';

interface ToolExecutionState {
  tools: Record<string, ToolExecution>;
  idsByOwner: Record<string, string[]>;
  ingest: (tool: ToolExecution) => void;
  replaceAll: (tools: ToolExecution[]) => void;
  hydrateConversation: (
    workspaceId: string,
    conversationId: string,
    tools: ToolExecution[]
  ) => void;
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

function executionIdentity(
  tool: Pick<
    ToolExecution,
    'workspace_id' | 'conversation_id' | 'owner' | 'call_id' | 'run_id' | 'name'
  >
): string {
  return JSON.stringify([
    tool.workspace_id,
    tool.conversation_id ?? null,
    tool.run_id ?? null,
    toolExecutionOwnerKey(tool.owner, tool.run_id),
    tool.call_id,
    tool.name,
  ]);
}

function toolStatusRank(tool: ToolExecution): number {
  // 后端会以Unknown结束孤儿调用；有结束时间不能继续显示running，也不能覆盖真实结果。
  if (tool.status === 'unknown') return tool.finished_at == null ? -1 : 1;
  return tool.status === 'running' ? 0 : 2;
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
  if (persisted.detail_ref) return persisted;
  return { ...mergeToolExecution(persisted, runtimeBoundary), id: persisted.id };
}

export function mergeHydratedToolExecutions(
  current: readonly ToolExecution[],
  incoming: readonly ToolExecution[]
): ToolExecution[] {
  const merged = new Map<string, ToolExecution>();
  for (const tool of [...current, ...incoming]) {
    // detail id才指向原执行；同call_id的多个详情不能在冷恢复中丢失歧义。
    const identity = JSON.stringify([executionIdentity(tool), tool.id]);
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
    const startedAt = previous ? Math.min(previous.started_at, timestamp) : timestamp;
    const boundary: ToolExecution = {
      id,
      workspace_id: run.workspace_id,
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
    };
    // 回放顺序可能包含迟到的开始；实时与冷恢复复用同一归并规则。
    tools.set(id, previous ? mergeToolExecution(previous, boundary) : boundary);
  }

  return [...tools.values()];
}

/** Keep full persisted details while treating TaskRuntime terminal facts as authoritative. */
export function mergeTaskRuntimeToolExecutions(
  persisted: readonly ToolExecution[],
  fallback: readonly ToolExecution[]
): ToolExecution[] {
  const merged = mergeHydratedToolExecutions([], persisted);
  const positions = new Map<string, number | null>();
  merged.forEach((tool, index) => {
    const identity = executionIdentity(tool);
    positions.set(identity, positions.has(identity) ? null : index);
  });
  for (const boundary of fallback) {
    const identity = executionIdentity(boundary);
    const position = positions.get(identity);
    if (position === undefined) {
      positions.set(identity, merged.length);
      merged.push(boundary);
    } else if (position !== null) {
      const existing = merged.at(position);
      if (existing) merged[position] = mergeTaskRuntimeBoundary(existing, boundary);
    }
    // 多个详情共享call_id时不挑选；原位更新保持冷恢复中的原执行顺序。
  }
  return merged;
}

export const useToolExecutionStore = create<ToolExecutionState>((set) => ({
  tools: {},
  idsByOwner: {},

  ingest: (tool) => {
    set((state) => {
      const existing = state.tools[tool.id];
      // 不同detail id可能是重复call_id的独立执行，必须保留歧义交原consumer判Unknown。
      const incoming =
        existing && executionIdentity(existing) === executionIdentity(tool)
          ? mergeToolExecution(existing, tool)
          : tool;
      const ownerKey = toolExecutionOwnerKey(incoming.owner, incoming.run_id);
      const previousOwnerKey = existing
        ? toolExecutionOwnerKey(existing.owner, existing.run_id)
        : null;
      let idsByOwner = state.idsByOwner;
      // 同一详情重新绑定owner时移除旧引用；普通事件继续原增量路径，避免重建全部索引。
      if (previousOwnerKey !== null && previousOwnerKey !== ownerKey) {
        idsByOwner = {
          ...idsByOwner,
          [previousOwnerKey]: (idsByOwner[previousOwnerKey] ?? []).filter((id) => id !== tool.id),
        };
      }
      const ownerIds = idsByOwner[ownerKey] ?? [];
      if (!ownerIds.includes(tool.id)) {
        idsByOwner = { ...idsByOwner, [ownerKey]: [...ownerIds, tool.id] };
      }
      return { tools: { ...state.tools, [tool.id]: incoming }, idsByOwner };
    });
  },

  replaceAll: (tools) => {
    set(() => indexTools(tools));
  },

  hydrateConversation: (workspaceId, conversationId, tools) => {
    set((state) => {
      const currentTools = Object.values(state.tools);
      const otherConversationTools = currentTools.filter(
        (tool) => tool.workspace_id !== workspaceId || tool.conversation_id !== conversationId
      );
      const liveTools = currentTools.filter(
        (tool) => tool.workspace_id === workspaceId && tool.conversation_id === conversationId
      );
      return indexTools([
        ...otherConversationTools,
        ...mergeHydratedToolExecutions(
          liveTools,
          tools.filter(
            (tool) => tool.workspace_id === workspaceId && tool.conversation_id === conversationId
          )
        ),
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
    const matches = Object.values(state.tools).filter(
      (tool) => executionIdentity(tool) === executionIdentity(projected)
    );
    if (matches.length > 1) continue;
    const existing = matches.at(0);
    useToolExecutionStore
      .getState()
      .ingest(existing ? mergeTaskRuntimeBoundary(existing, projected) : projected);
  }
}
