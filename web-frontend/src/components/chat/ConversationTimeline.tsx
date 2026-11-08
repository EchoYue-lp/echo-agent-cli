import type { ComponentProps } from 'react';
import {
  conversationTimeline,
  type ConversationTimelineItem,
} from '../../lib/conversationTimeline';
import { useToolExecutionStore } from '../../stores/toolExecutionStore';
import { MessageBubble } from './MessageBubble';
import { NativeMessageTimeline, projectNativeTimeline } from './NativeMessageTimeline';
import { ExecutionProcessGroup } from './ExecutionProcessGroup';
import { ChartCard } from './ChartCard';
import { SubagentStreamBlock } from './SubagentStreamBlock';
import { useSubagentRunStore } from '../../stores/subagentRunStore';
import { recordBelongsToTurn, uniquePresentationRecords } from '../../lib/nativePresentation';
import { toolExecutionGroupPresentation } from './ToolExecutionGroup';
import type { ChatMessage } from '../../types/api';

type MessageActions = Pick<
  ComponentProps<typeof MessageBubble>,
  'onRegenerate' | 'onEditAndResend' | 'onFork' | 'forkDisabled'
>;

interface ConversationTimelineProps extends MessageActions {
  messages: readonly ChatMessage[];
  containerKey: string;
  forkReplies?: ReadonlyMap<string, number>;
  targetScope?: { workspaceId: string; conversationId: string | null };
}

function turnFinalPresentation(item: Extract<ConversationTimelineItem, { kind: 'turn' }>) {
  const finals = item.messages.filter(
    (message) => message.finalAnswerConfirmed && message.content.trim()
  );
  const final = finals.length === 1 ? finals.at(0) : undefined;
  const qualifiedFinal =
    final?.finalAnswerExecutionId === item.source.execution_id ? final : undefined;
  return { qualifiedFinal, conflicted: finals.length > 0 && !qualifiedFinal };
}

function TurnTimeline({
  item,
  actions,
  forkReplies,
  targetScope,
}: {
  item: Extract<ConversationTimelineItem, { kind: 'turn' }>;
  actions: MessageActions;
  forkReplies?: ReadonlyMap<string, number>;
  targetScope?: { workspaceId: string; conversationId: string | null };
}) {
  const tools = useToolExecutionStore((state) => state.tools);
  const runs = useSubagentRunStore((state) => state.runs);
  const projection = projectNativeTimeline(item.messages, Object.values(runs), tools);
  const charts = uniquePresentationRecords(
    item.messages.flatMap((message) => message.presentationRecords ?? [])
  ).flatMap((record) =>
    record.payload.kind === 'chart' && recordBelongsToTurn(record, item.source)
      ? [{ id: record.event_id, spec: record.payload.data.spec }]
      : []
  );
  const { qualifiedFinal, conflicted } = turnFinalPresentation(item);
  const ids = [
    ...new Set(
      item.messages.flatMap(
        (message) =>
          message.executionSteps?.flatMap((step) => (step.type === 'tool' ? [step.callId] : [])) ??
          []
      )
    ),
  ];
  const presentation = toolExecutionGroupPresentation(
    ids,
    tools,
    item.messages.flatMap((message) => message.transcriptTools ?? [])
  );
  // 来源冲突不抑制正文，也不能凭“最后一条”替代精确最终发布关联。
  if (conflicted)
    return (
      <>
        <div role="status" className="py-1 text-xs text-[var(--text-tertiary)]">
          最终结果关联未恢复
        </div>
        {item.messages.map((message) => (
          <MessageBubble
            key={message.id}
            message={message}
            hideParallel
            {...actions}
            onFork={forkReplies?.has(message.id) ? actions.onFork : undefined}
          />
        ))}
      </>
    );
  return (
    <section className="min-w-0 py-1" aria-label="助手轮次" data-turn-key={item.key}>
      <ExecutionProcessGroup
        completed={Boolean(qualifiedFinal)}
        statusLabel="最终结果已发布"
        unrestored={
          presentation.missingCount > 0 ||
          presentation.uncertainCount > 0 ||
          projection.unplaced.length > 0 ||
          item.messages.some((message) => message.presentationUnavailable)
        }
        toolFailureCount={
          presentation.failedCount +
          projection.subagents.filter(
            (run) => run.status === 'failed' || run.status === 'timed_out'
          ).length
        }
      >
        <NativeMessageTimeline
          messages={item.messages}
          projection={projection}
          targetScope={targetScope}
        />
      </ExecutionProcessGroup>
      {item.messages.some((message) => message.presentationUnavailable) && (
        <p role="status" className="py-1 text-xs text-[var(--text-tertiary)]">
          部分图表或执行位置未恢复
        </p>
      )}
      {projection.subagents
        .filter((run) => (run.status === 'failed' || run.status === 'timed_out') && run.error)
        .map((run) => (
          <p
            key={`${run.runId}:${run.subagentRunId}:failure`}
            role="status"
            className="py-1 text-xs text-[var(--color-error)]"
          >
            子代理 {run.agent} 的失败记录：{run.error}
          </p>
        ))}
      {item.messages
        .filter((message) => message.id !== qualifiedFinal?.id)
        .map((message) => (
          <MessageBubble
            key={message.id}
            message={message}
            hideParallel
            hideProcess
            hideBody={!message.failureNotice && message.executionStatus?.status !== 'failed'}
          />
        ))}
      {qualifiedFinal && (
        <MessageBubble
          message={qualifiedFinal}
          hideParallel
          hideProcess
          {...actions}
          onFork={forkReplies?.has(qualifiedFinal.id) ? actions.onFork : undefined}
        />
      )}
      {charts.map((chart) => (
        <ChartCard key={chart.id} spec={chart.spec} />
      ))}
    </section>
  );
}

/** 实时和冷历史共享展示投影；不写canonical历史或运行状态。 */
export function ConversationTimeline({
  messages,
  containerKey,
  forkReplies,
  targetScope,
  ...actions
}: ConversationTimelineProps) {
  const items = conversationTimeline(messages, containerKey);
  const runs = useSubagentRunStore((state) => state.runs);
  const sources = items.flatMap((item) =>
    item.kind === 'turn' && !turnFinalPresentation(item).conflicted ? [item.source] : []
  );
  const records = uniquePresentationRecords(
    messages.flatMap((message) => message.presentationRecords ?? [])
  );
  const unassociatedCharts = records.flatMap((record) =>
    record.payload.kind === 'chart' &&
    !sources.some((source) => recordBelongsToTurn(record, source))
      ? [{ id: record.event_id, spec: record.payload.data.spec }]
      : []
  );
  const unassociatedRunKeys = new Set(
    records.flatMap((record) =>
      record.payload.kind === 'subagent' &&
      !sources.some((source) => recordBelongsToTurn(record, source))
        ? [
            JSON.stringify([
              record.payload.data.event.run_id,
              record.payload.data.event.subagent_run_id,
            ]),
          ]
        : []
    )
  );
  const unassociatedRuns = Object.values(runs).filter((run) => {
    const inTargetConversation =
      targetScope &&
      run.workspaceId === targetScope.workspaceId &&
      run.conversationId === targetScope.conversationId;
    const hasKnownParent = sources.some(
      (source) =>
        run.workspaceId === source.workspace_id &&
        run.conversationId === source.conversation_id &&
        run.events.some(
          (event) => event.framework_event?.parent_execution_id === source.execution_id
        )
    );
    return (
      !hasKnownParent &&
      (inTargetConversation ||
        unassociatedRunKeys.has(JSON.stringify([run.runId, run.subagentRunId])))
    );
  });
  return (
    <>
      {items.map((item, index) => {
        const message = item.kind === 'turn' ? item.messages.at(0) : item.message;
        const previous = items.at(index - 1);
        const previousMessage =
          previous?.kind === 'turn' ? previous.messages.at(-1) : previous?.message;
        const separator =
          index > 0 &&
          message &&
          previousMessage &&
          message.timestamp - previousMessage.timestamp > 300000;
        return (
          <div key={item.key}>
            {separator && (
              <div className="flex items-center gap-3 py-2">
                <div className="h-px flex-1 bg-[var(--border-secondary)]" />
                <span className="text-xs text-[var(--text-tertiary)]">
                  {new Date(message.timestamp).toLocaleTimeString([], {
                    hour: '2-digit',
                    minute: '2-digit',
                  })}
                </span>
                <div className="h-px flex-1 bg-[var(--border-secondary)]" />
              </div>
            )}
            {item.kind === 'turn' ? (
              <TurnTimeline
                item={item}
                actions={actions}
                forkReplies={forkReplies}
                targetScope={targetScope}
              />
            ) : (
              <>
                {item.associationUnavailable && (
                  <div role="status" className="text-xs text-[var(--text-tertiary)]">
                    轮次关联未恢复
                  </div>
                )}
                <MessageBubble
                  message={item.message}
                  hideParallel
                  {...actions}
                  onFork={forkReplies?.has(item.message.id) ? actions.onFork : undefined}
                />
              </>
            )}
          </div>
        );
      })}
      {unassociatedCharts.map((chart) => (
        <div key={chart.id}>
          <p role="status" className="text-xs text-[var(--text-tertiary)]">
            图表关联未恢复
          </p>
          <ChartCard spec={chart.spec} />
        </div>
      ))}
      {unassociatedRuns.length > 0 && (
        <details className="py-1 text-xs text-[var(--text-tertiary)]">
          <summary className="cursor-pointer">
            子代理归属未恢复 · {unassociatedRuns.length} 个
          </summary>
          {unassociatedRuns.map((run) => (
            <SubagentStreamBlock
              key={`${run.runId}:${run.subagentRunId}`}
              run={run}
              readOnlySource
            />
          ))}
        </details>
      )}
    </>
  );
}
