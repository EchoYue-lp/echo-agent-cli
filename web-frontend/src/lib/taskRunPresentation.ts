import type { TaskRun } from '../generated';

/** 只读来源标签：命名空间由 EKO 的渠道/occurrence 入口拥有，不改变 Run 状态。 */
export function taskRunSourceLabel(run: Pick<TaskRun, 'conversation_id'>): string {
  if (run.conversation_id.startsWith('channel:')) return 'IM';
  if (run.conversation_id.startsWith('cron:')) return '定时';
  return '对话';
}
