import { useState } from 'react';
import { conversationApi, schedulerApi } from '../../api/endpoints';
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
import { useContextPaneStore } from '../../stores/contextPaneStore';
import { useWorkspaceViewStore } from '../../stores/workspaceViewStore';
import { productDataScope, sameProductDataScope } from '../../lib/productDataScope';
import { WeixinNotificationResult } from './WeixinNotification';

export function SchedulerRunLink({ run }: { run: SchedulerRunReference }) {
  const open = async () => {
    const epoch = useUiStore.getState().beginNavigation();
    try {
      const workspaces = useWorkspaceStore.getState();
      const target = workspaces.workspaces.find((workspace) => workspace.id === run.workspace_id);
      if (run.workspace_id !== 'global' && !target) throw new Error('原任务工作区已不可用');
      const scope = productDataScope(target);
      const transition =
        (workspaces.current?.id ?? 'global') === run.workspace_id
          ? null
          : run.workspace_id === 'global'
            ? workspaces.exit()
            : workspaces.switchTo(run.workspace_id);
      // 与侧栏相同，切换同步分离旧会话后捕获新稿代次；后继导航不接收迟到结果。
      const draftEpoch = useConversationStore.getState().newConversationEpoch;
      const canOpen = () =>
        useUiStore.getState().navigationEpoch === epoch &&
        useConversationStore.getState().newConversationEpoch === draftEpoch &&
        useConversationStore.getState().workspaceId === scope.workspaceId &&
        sameProductDataScope(productDataScope(useWorkspaceStore.getState().current), scope);
      if (transition) await transition;
      if (!canOpen()) return;
      // Run 可早于 transcript 失败。只按原元数据确认存在性，不把读取错误当成不存在。
      const conversations = await conversationApi.list(scope.workspaceId);
      if (!canOpen()) return;
      if (conversations.some((item) => item.conversation_id === run.conversation_id)) {
        const projected = await useConversationStore
          .getState()
          .loadConversation(run.conversation_id, canOpen);
        if (!projected || !canOpen()) return;
        if (useConversationStore.getState().activeId !== run.conversation_id)
          throw new Error('原会话尚未载入；原任务记录保留');
      }
      await useTaskRuntimeStore.getState().loadByRun(scope, run.run_id);
      if (!canOpen()) return;
      const tasks = useTaskRuntimeStore.getState();
      if (tasks.historyError) throw new Error(tasks.historyError);
      const loaded = tasks.historySnapshot?.activeRun;
      if (
        tasks.selectedRunId !== run.run_id ||
        !tasks.taskScope ||
        !sameProductDataScope(tasks.taskScope, scope) ||
        loaded?.run_id !== run.run_id ||
        loaded.workspace_id !== run.workspace_id ||
        loaded.conversation_id !== run.conversation_id
      )
        throw new Error('原任务结果尚未载入或身份不匹配');
      useWorkspaceViewStore.getState().openChat();
      useContextPaneStore.getState().openTasks();
      useUiStore.getState().closeSettings();
    } catch (error) {
      if (useUiStore.getState().navigationEpoch === epoch)
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
