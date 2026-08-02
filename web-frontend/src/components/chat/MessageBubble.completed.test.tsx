import { renderToStaticMarkup } from 'react-dom/server';
import { beforeEach, describe, expect, it } from 'vitest';
import { useSubagentRunStore } from '../../stores/subagentRunStore';
import type { ChatMessage } from '../../types/api';
import { MessageBubble } from './MessageBubble';

describe('MessageBubble completed execution', () => {
  beforeEach(() => {
    useSubagentRunStore.getState().clear();
  });

  it('shows only the collapsed process summary before the final result', () => {
    const message: ChatMessage = {
      id: 'assistant-finished',
      role: 'assistant',
      content: '最终结果正文',
      timestamp: 1,
      isStreaming: false,
      thinkingSegments: [{ content: '不应默认展开的思考' }],
      executionSteps: [
        { type: 'thinking', index: 0 },
        { type: 'tool', callId: 'task-execute-call' },
      ],
    };
    useSubagentRunStore.getState().ingest({
      kind: 'subagent',
      subagent_run_id: 'task-1:1:1',
      run_id: 'run-1',
      task_id: 'task-1',
      agent: 'explorer',
      event: 'completed',
      message_id: message.id,
      summary: '不应默认展开的 Subagent 结果',
    });

    const markup = renderToStaticMarkup(<MessageBubble message={message} />);

    expect(markup).toContain('aria-label="展开执行过程"');
    expect(markup).toContain('最终结果正文');
    expect(markup).not.toContain('不应默认展开的思考');
    expect(markup).not.toContain('不应默认展开的 Subagent 结果');
  });
});
