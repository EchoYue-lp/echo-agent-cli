import type { AssistantBlockView, ChatMessage } from '../../types/api';
import MarkdownContent from '../common/MarkdownContent';
import { ThinkingSegment } from './ThinkingSegment';
import { ToolExecutionGroup } from './ToolExecutionGroup';

/** 只读取原 native 块的公开文字；签名、redacted 和 opaque data 不作正文回退。 */
export function nativeAssistantBlockText(view: AssistantBlockView): string {
  if (view.content.trim() || view.block.phase !== 'reasoning') return view.content;
  const reasoning = view.block.reasoning;
  if (!reasoning || typeof reasoning !== 'object') return '';
  const value = reasoning as { type?: unknown; thinking?: unknown; summary?: unknown };
  if (value.type === 'signed' && typeof value.thinking === 'string') return value.thinking;
  if (
    value.type === 'opaque' &&
    Array.isArray(value.summary) &&
    value.summary.every((part) => typeof part === 'string')
  )
    return value.summary.join('\n\n');
  return '';
}

/** 只呈现显式消息块及原工具引用；执行终态始终来自运行时。 */
export function NativeMessageTimeline({ message }: { message: ChatMessage }) {
  const blocks = message.assistantBlocks ?? [];
  const steps =
    message.executionSteps ?? blocks.map((_, index) => ({ type: 'assistant' as const, index }));
  const status = message.executionStatus;
  const source = message.finalAnswerSource;
  const selected =
    message.finalAnswerConfirmed &&
    status?.status === 'settled' &&
    source &&
    message.finalAnswerExecutionId === source.execution_id &&
    status.execution_id === source.execution_id &&
    status.request_id === source.request_id &&
    status.sequence === source.sequence
      ? source
      : undefined;
  const renderedBlockIds = new Set<string>();
  return (
    <div className="space-y-2" aria-label="消息与工具时间线">
      {steps.map((step, index) => {
        if (step.type === 'tool') {
          return (
            <ToolExecutionGroup
              key={`tool-${index}`}
              toolIds={[step.callId]}
              transcriptTools={message.transcriptTools}
            />
          );
        }
        if (step.type !== 'assistant') return null;
        const view = blocks[step.index];
        if (!view) return null;
        const { block } = view;
        const content = nativeAssistantBlockText(view);
        if (!content.trim()) return null;
        // 原生候选不具有发布资格；确认后的最终正文由唯一 final_answer 事件呈现。
        if (block.phase === 'final_answer') return null;
        // Unknown 仅按原 Settled 三元及后端已核验 approved block 精确身份折叠。
        // 显式进度/Reasoning 与缺少来源的块都保留，不从正文、位置或 complete 推身份。
        if (
          block.phase === 'unknown' &&
          selected &&
          view.execution_id === selected.execution_id &&
          selected.blocks.some(
            (identity) =>
              identity.request_id === block.request_id &&
              identity.block_id === block.block_id &&
              identity.sequence === block.sequence &&
              identity.publication_event_id === view.publication_event_id
          )
        )
          return null;
        const identity = JSON.stringify([
          view.execution_id ?? null,
          block.request_id,
          block.block_id,
        ]);
        if (renderedBlockIds.has(identity)) return null;
        renderedBlockIds.add(identity);
        if (block.phase === 'reasoning') {
          return (
            <ThinkingSegment
              key={identity}
              index={1}
              total={1}
              kind="reasoning"
              content={content}
            />
          );
        }
        return (
          <div key={identity} className="text-sm leading-relaxed text-[var(--text-assistant-msg)]">
            <MarkdownContent className="break-words" content={content} />
          </div>
        );
      })}
    </div>
  );
}
