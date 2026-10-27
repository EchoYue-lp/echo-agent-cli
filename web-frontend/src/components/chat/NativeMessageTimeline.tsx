import type { ChatMessage } from '../../types/api';
import MarkdownContent from '../common/MarkdownContent';
import { ThinkingSegment } from './ThinkingSegment';
import { ToolExecutionGroup } from './ToolExecutionGroup';

/** 只呈现显式消息块及原工具引用；执行终态始终来自运行时。 */
export function NativeMessageTimeline({ message }: { message: ChatMessage }) {
  const blocks = message.assistantBlocks ?? [];
  const steps =
    message.executionSteps ?? blocks.map((_, index) => ({ type: 'assistant' as const, index }));
  const last = steps.at(-1);
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
        if (!view?.content.trim()) return null;
        const { block, content } = view;
        // 原生候选不具有发布资格；确认后的最终正文由唯一 final_answer 事件呈现。
        if (block.phase === 'final_answer') return null;
        // 无阶段协议可能先发同一份正文，再发布最终事实。这只去除显示重复，
        // 不从文字匹配推导任何完成或阶段，候选/终态均保持原事实。
        if (
          message.finalAnswerConfirmed &&
          last?.type === 'assistant' &&
          last.index === step.index &&
          content === message.content
        )
          return null;
        if (block.phase === 'reasoning') {
          return (
            <ThinkingSegment
              key={`${block.request_id}:${block.block_id}`}
              index={1}
              total={1}
              kind="reasoning"
              content={content}
            />
          );
        }
        return (
          <div
            key={`${block.request_id}:${block.block_id}`}
            className="text-sm leading-relaxed text-[var(--text-assistant-msg)]"
          >
            <MarkdownContent className="break-words" content={content} />
          </div>
        );
      })}
    </div>
  );
}
