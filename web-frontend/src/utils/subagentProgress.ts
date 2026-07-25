import type { SubagentRunStatus } from '../stores/subagentRunStore';

export function statusLabel(status: SubagentRunStatus): string {
  switch (status) {
    case 'running':
      return '运行中';
    case 'completed':
      return '已完成';
    case 'failed':
      return '失败';
    case 'cancelled':
      return '已取消';
    case 'timed_out':
      return '已超时';
  }
}
