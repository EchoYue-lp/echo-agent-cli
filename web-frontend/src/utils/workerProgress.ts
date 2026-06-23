// Worker progress summary computation (spec §3.3.4)
import type { WorkerTraceState, WorkerTraceEvent } from '../stores/workerTraceStore';

/** Tool names considered "read" operations (exploration). Frontend heuristic set. */
const READ_TOOL_NAMES = new Set([
  'read_file', 'read', 'read_files',
  'glob', 'grep', 'rg', 'search',
  'list', 'list_files', 'ls', 'list_dir',
  'view', 'cat', 'head', 'tail',
]);

export interface WorkerProgress {
  status: 'running' | 'completed' | 'failed' | 'cancelled' | 'planned';
  toolCount: number;
  readCount: number;
  thinkingRounds: number;
}

/** Count `worker_tool_start` events with a read-like tool name. */
function countReadTools(events: WorkerTraceEvent[]): number {
  return events.filter(
    (e) =>
      e.event_type === 'worker_tool_start' &&
      READ_TOOL_NAMES.has(String((e.payload as Record<string, unknown> | null)?.name ?? '').toLowerCase())
  ).length;
}

export function computeWorkerProgress(worker: WorkerTraceState): WorkerProgress {
  const events = worker.events;
  const toolCount = events.filter((e) => e.event_type === 'worker_tool_start').length;
  const readCount = countReadTools(events);
  const thinkingRounds = events.filter((e) => e.event_type === 'worker_thinking_end').length;
  return {
    status: worker.status,
    toolCount,
    readCount,
    thinkingRounds,
  };
}

export function progressSummary(p: WorkerProgress): string {
  if (p.status === 'failed' || p.status === 'cancelled') {
    return p.status === 'failed' ? '失败' : '已取消';
  }
  const parts: string[] = [];
  if (p.toolCount > 0) parts.push(`${p.toolCount} 工具`);
  if (p.readCount > 0) parts.push(`已读 ${p.readCount}`);
  if (p.thinkingRounds > 0) parts.push(`思考 ${p.thinkingRounds} 轮`);
  return parts.join(' · ');
}

export function statusLabel(status: WorkerProgress['status']): string {
  switch (status) {
    case 'running': return '运行中';
    case 'completed': return '已完成';
    case 'failed': return '失败';
    case 'cancelled': return '已取消';
    case 'planned': return '已规划';
  }
}
