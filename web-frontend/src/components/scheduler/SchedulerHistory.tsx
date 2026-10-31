import { useState } from 'react';
import { schedulerApi } from '../../api/endpoints';
import type {
  SchedulerHistoryCursor,
  SchedulerHistoryRecord,
  SchedulerRunLookup,
  SchedulerRunReference,
} from '../../api/endpoints';
import { useToastStore } from '../../stores/toastStore';
import { useConversationStore } from '../../stores/conversationStore';
import { useUiStore } from '../../stores/uiStore';
import { useWorkspaceStore } from '../../stores/workspaceStore';
import { useTaskRuntimeStore } from '../../stores/taskRuntimeStore';
import { WeixinNotificationResult } from './WeixinNotification';

export function SchedulerRunLink({ run }: { run: SchedulerRunReference }) {
  const open = async () => {
    try {
      const workspaces = useWorkspaceStore.getState();
      if ((workspaces.current?.id ?? 'global') !== run.workspace_id) {
        if (run.workspace_id === 'global') await workspaces.exit();
        else await workspaces.switchTo(run.workspace_id);
      }
      if ((useWorkspaceStore.getState().current?.id ?? 'global') !== run.workspace_id)
        throw new Error('工作区切换未确认');
      await useConversationStore.getState().loadConversation(run.conversation_id);
      await useTaskRuntimeStore.getState().refresh(run.workspace_id, run.run_id);
      if (useTaskRuntimeStore.getState().activeRun?.run_id !== run.run_id)
        throw new Error('原任务结果尚未载入');
      useUiStore.getState().closeSettings();
    } catch (error) {
      useToastStore
        .getState()
        .addToast('error', error instanceof Error ? error.message : String(error));
    }
  };
  return (
    <div className="space-y-2">
      <button className="text-xs underline" onClick={() => void open()}>
        查看任务结果 {run.run_id}
      </button>
      <WeixinNotificationResult runId={run.run_id} />
    </div>
  );
}

/** A view of explicitly read original journal pages, never a completion reducer. */
export function SchedulerHistory() {
  const [records, setRecords] = useState<SchedulerHistoryRecord[]>([]);
  const [runs, setRuns] = useState<Record<string, SchedulerRunLookup>>({});
  const [cursor, setCursor] = useState<SchedulerHistoryCursor | null>(null);
  const [opened, setOpened] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [taskId, setTaskId] = useState('');
  const read = async (restart: boolean) => {
    setBusy(true);
    setError(null);
    try {
      const response = await schedulerApi.history(restart ? null : cursor);
      if (!response.success) {
        setError(
          `${response.cause}${response.error ? `: ${response.error}` : ''}；请显式刷新历史边界`
        );
        return;
      }
      setRecords((previous) =>
        restart ? response.page.events : [...previous, ...response.page.events]
      );
      setRuns((previous) => (restart ? response.runs : { ...previous, ...response.runs }));
      setCursor(response.page.next_cursor);
      setOpened(true);
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : String(failure));
    } finally {
      setBusy(false);
    }
  };
  const definitions = new Map(
    records.flatMap((record) =>
      record.event.envelope
        ? [[record.event.envelope.message_id, record.event.envelope.payload.task] as const]
        : []
    )
  );
  const taskNames = new Map([...definitions.values()].map((task) => [task.id, task.name]));
  const groups = new Map<string, SchedulerHistoryRecord[]>();
  for (const record of records) {
    const id = record.event.envelope?.message_id ?? record.event.message_id;
    if (!id || (taskId && definitions.get(id)?.id !== taskId)) continue;
    groups.set(id, [...(groups.get(id) ?? []), record]);
  }
  return (
    <section
      className="space-y-2 border-t pt-3 text-xs"
      style={{ borderColor: 'var(--border-primary)' }}
    >
      <div className="flex items-center gap-3">
        <button disabled={busy} onClick={() => void read(true)}>
          {opened ? '刷新运行历史' : '读取运行历史'}
        </button>
        {opened && (
          <select
            aria-label="筛选历史任务"
            value={taskId}
            onChange={(event) => setTaskId(event.target.value)}
          >
            <option value="">所有已读任务</option>
            {[...taskNames].map(([id, name]) => (
              <option key={id} value={id}>
                {name}
              </option>
            ))}
          </select>
        )}
      </div>
      {error && <p role="alert">{error}</p>}
      {opened && (
        <p>
          已读取 {records.length} 条日志事实；
          {cursor ? '还有未读页，当前结果不代表完整历史。' : '已到本次日志快照末尾。'}
        </p>
      )}
      {[...groups].map(([id, events]) => (
        <details
          key={id}
          className="rounded border p-2"
          style={{ borderColor: 'var(--border-primary)' }}
        >
          <summary>
            {definitions.get(id)?.name ?? '任务归属未在已读页确认'} · {id}
          </summary>
          <p>定义：{definitions.get(id)?.definition_id || '尚未读到原定义'}</p>
          <ol>
            {events.map((record) => (
              <li key={record.sequence}>
                #{record.sequence} {record.event.kind}
                {record.event.outcome ? ` · ${record.event.outcome}` : ''}
              </li>
            ))}
          </ol>
          {runs[id]?.run ? (
            <SchedulerRunLink run={runs[id].run} />
          ) : (
            <p>尚无可定位的 TaskRun（{runs[id]?.lookup ?? '尚未读取'}）</p>
          )}
        </details>
      ))}
      {opened && groups.size === 0 && <p>已读页没有匹配任务；未读页或缺失记录不表示运行完成。</p>}
      {cursor && (
        <button disabled={busy || !!error} onClick={() => void read(false)}>
          加载下一页
        </button>
      )}
    </section>
  );
}
