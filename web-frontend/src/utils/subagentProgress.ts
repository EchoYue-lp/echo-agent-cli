// Subagent progress summary computation (spec §3.3.4).
//
// Originally computed from WorkerTraceState/WorkerTraceEvent; Phase 3 of the
// Subagent unification rewires it onto SubagentRunState/ExecutionEvent. The
// event-type strings dropped their `worker_` prefix (`tool_started` not
// `worker_tool_start`) and fields moved from `payload.*` to the event top
// level (`e.name` not `e.payload.name`).
import type { SubagentRunState, ExecutionEvent } from '../stores/subagentRunStore';

/** Tool names considered "read" operations (exploration). Frontend heuristic set. */
const READ_TOOL_NAMES = new Set([
  'read_file',
  'read',
  'read_files',
  'glob',
  'grep',
  'rg',
  'search',
  'list',
  'list_files',
  'ls',
  'list_dir',
  'view',
  'cat',
  'head',
  'tail',
]);

export interface SubagentProgress {
  status: 'running' | 'completed' | 'failed' | 'cancelled';
  toolCount: number;
  readCount: number;
  thinkingRounds: number;
}

/** Count `tool_started` events with a read-like tool name. */
function countReadTools(events: ExecutionEvent[]): number {
  return events.filter(
    (e) => e.event === 'tool_started' && READ_TOOL_NAMES.has(String(e.name ?? '').toLowerCase())
  ).length;
}

export function computeSubagentProgress(run: SubagentRunState): SubagentProgress {
  const events = run.events;
  const toolCount = events.filter((e) => e.event === 'tool_started').length;
  const readCount = countReadTools(events);
  // `usage` events correspond to thinking_ended (one round of thinking per
  // model call). thinking_ended itself also maps to `usage`, so this counts
  // completed thinking rounds.
  const thinkingRounds = events.filter((e) => e.event === 'usage').length;
  return {
    status: run.status,
    toolCount,
    readCount,
    thinkingRounds,
  };
}

export function progressSummary(p: SubagentProgress): string {
  if (p.status === 'failed' || p.status === 'cancelled') {
    // Status text is already shown by `statusLabel` next to this summary;
    // returning it here too would render "失败 · 失败". Return empty so only
    // the tool/read/thinking counts appear (or nothing for failed/cancelled).
    return '';
  }
  const parts: string[] = [];
  if (p.toolCount > 0) parts.push(`${p.toolCount} 工具`);
  if (p.readCount > 0) parts.push(`已读 ${p.readCount}`);
  if (p.thinkingRounds > 0) parts.push(`思考 ${p.thinkingRounds} 轮`);
  return parts.join(' · ');
}

export function statusLabel(status: SubagentProgress['status']): string {
  switch (status) {
    case 'running':
      return '运行中';
    case 'completed':
      return '已完成';
    case 'failed':
      return '失败';
    case 'cancelled':
      return '已取消';
  }
}
