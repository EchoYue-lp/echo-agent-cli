import type { AgentEventEnvelope } from '../../types/api';
import { ThinkingSegment } from '../chat/ThinkingSegment';

/** Only presents original events; completion still belongs to task/subagent lifecycle. */
export function SubagentNativeEvent({
  envelope,
  content: combined,
  originals,
}: {
  envelope: AgentEventEnvelope;
  content?: string;
  originals?: AgentEventEnvelope[];
}) {
  const event = envelope.payload;
  if (event.type === 'assistant_message') {
    const { block } = event.data;
    // A provider candidate is retained in the journal but has no release authority.
    if (block.phase === 'final_answer') return null;
    const sourceReasoning = (originals ?? [envelope]).flatMap((source) =>
      source.payload.type === 'assistant_message' ? [source.payload.data.block.reasoning] : []
    );
    const thinking = sourceReasoning
      .flatMap((value) => {
        const raw = value as { thinking?: unknown } | null;
        return typeof raw?.thinking === 'string' ? [raw.thinking] : [];
      })
      .join('');
    const content = combined || block.delta || thinking;
    return (
      <div
        data-block-id={block.block_id}
        data-request-id={block.request_id}
        data-phase={block.phase}
      >
        {block.phase === 'reasoning' ? (
          <ThinkingSegment
            kind="reasoning"
            content={content || '已保留原生推理状态'}
            index={1}
            total={1}
          />
        ) : block.phase === 'unknown' ? (
          <details>
            <summary>阶段未标注的原生消息</summary>
            <pre className="whitespace-pre-wrap">{content}</pre>
          </details>
        ) : content ? (
          <p className="whitespace-pre-wrap break-words text-sm">{content}</p>
        ) : null}
        <details className="text-xs text-[var(--text-tertiary)]">
          <summary>原生消息身份与来源</summary>
          <pre className="overflow-auto whitespace-pre-wrap">
            {JSON.stringify(originals ?? [envelope], null, 2)}
          </pre>
        </details>
      </div>
    );
  }
  if (event.type === 'execution_status')
    return (
      <p role="status" className="text-xs text-[var(--text-secondary)]">
        执行观察：{event.data.status}
        {event.data.reason ? ` · ${event.data.reason}` : ''}
      </p>
    );
  if (event.type === 'final_answer')
    return <p className="whitespace-pre-wrap break-words text-sm">{event.data}</p>;
  if (event.type === 'cancelled')
    return <p role="status">Agent 执行已取消，任务状态沿原结算确认</p>;
  if (event.type === 'error') return <p role="alert">{event.data.message}</p>;
  return null;
}
