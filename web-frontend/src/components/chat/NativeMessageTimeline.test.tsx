// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { useChatStore } from '../../stores/chatStore';
import { useTaskRuntimeStore } from '../../stores/taskRuntimeStore';
import { useSubagentRunStore } from '../../stores/subagentRunStore';
import { useToolExecutionStore } from '../../stores/toolExecutionStore';
import type { AssistantMessageBlock } from '../../types/api';
import { MessageBubble } from './MessageBubble';
import { chatMessagesToSaved } from '../../stores/conversationStore';

function block(
  block_id: string,
  phase: AssistantMessageBlock['phase'],
  delta: string,
  sequence: number,
  complete = true
): AssistantMessageBlock {
  return {
    block_id,
    request_id: 'request',
    sequence,
    phase,
    delta,
    complete,
    provider: 'fixture',
    reasoning: null,
  };
}

describe('原生消息时间线', () => {
  beforeEach(() => {
    useChatStore.getState().clearMessages();
    useTaskRuntimeStore.getState().reset();
    useSubagentRunStore.getState().clear();
    useToolExecutionStore.getState().clear();
    useChatStore.getState().startAssistantMessage('native');
  });
  afterEach(cleanup);

  it('正文、工具和后续正文按原序显示，Reasoning 独立折叠', () => {
    const store = useChatStore.getState();
    store.appendAssistantBlock('native', block('r', 'reasoning', '原生思考摘要', 0));
    store.appendAssistantBlock('native', block('a', 'commentary', '先检查文件。', 1));
    store.recordToolStart('native', 'tool-1');
    store.appendAssistantBlock('native', block('b', 'partial_answer', '发现配置冲突。', 2));
    const message = useChatStore.getState().messages.at(-1);
    if (!message) throw new Error('missing assistant');
    const { container } = render(<MessageBubble message={message} />);
    const text = container.textContent ?? '';
    expect(text.indexOf('先检查文件。')).toBeLessThan(text.indexOf('状态未恢复'));
    expect(text.indexOf('状态未恢复')).toBeLessThan(text.indexOf('发现配置冲突。'));
    expect(screen.queryByText('原生思考摘要')).toBeNull();
    fireEvent.click(screen.getByRole('button', { name: '展开思考' }));
    expect(screen.getByText('原生思考摘要')).toBeTruthy();
    expect(message.content).toBe('');
    expect(message.isStreaming).toBe(true);
  });

  it('候选块即便 complete 也不公开最终正文或赋予分叉资格', () => {
    const store = useChatStore.getState();
    store.appendAssistantBlock('native', block('candidate', 'final_answer', '尚未校验的候选', 0));
    store.settleAssistantMessage('native');
    const message = useChatStore.getState().messages.at(-1);
    if (!message) throw new Error('missing assistant');
    const { rerender } = render(<MessageBubble message={message} onFork={vi.fn()} />);
    expect(screen.queryByText('尚未校验的候选')).toBeNull();
    expect(screen.queryByRole('button', { name: '分叉会话' })).toBeNull();
    store.applyFinalAnswer('native', '运行时确认后的最终回答');
    const confirmed = useChatStore.getState().messages.at(-1);
    if (!confirmed) throw new Error('missing confirmed assistant');
    rerender(<MessageBubble message={confirmed} onFork={vi.fn()} />);
    expect(screen.queryByText('尚未校验的候选')).toBeNull();
    expect(screen.getByText('运行时确认后的最终回答')).toBeTruthy();
    expect(screen.getByRole('button', { name: '分叉会话' })).toBeTruthy();
  });

  it('增量身份不混请求，完成后拒绝迟到增量，序列与原始字段无损进入 UI DTO', () => {
    const store = useChatStore.getState();
    store.appendAssistantBlock('native', block('same', 'commentary', '开始', 4, false));
    store.appendAssistantBlock('native', block('same', 'commentary', '继续', 4));
    store.appendAssistantBlock('native', block('same', 'commentary', '迟到', 4));
    store.appendAssistantBlock('native', {
      ...block('same', 'unknown', '另一请求', 0),
      request_id: 'other',
    });
    const message = useChatStore.getState().messages.at(-1);
    expect(message?.assistantBlocks?.map((view) => view.content)).toEqual(['开始继续', '另一请求']);
    const saved = chatMessagesToSaved(useChatStore.getState().messages).at(0);
    expect(saved?.assistant_blocks).toEqual(message?.assistantBlocks);
    expect(saved?.execution_steps).toEqual([
      { type: 'assistant', index: 0 },
      { type: 'assistant', index: 1 },
    ]);
    // DTO 往返证明不等同于后端持久化资格；后端仍须消费 canonical 来源。
  });

  it('最终发布幂等且拒绝空内容，不用后续冲突回答改写已发布正文', () => {
    const store = useChatStore.getState();
    store.applyFinalAnswer('native', '');
    expect(useChatStore.getState().messages.at(-1)?.finalAnswerConfirmed).not.toBe(true);
    store.applyFinalAnswer('native', '最终报告');
    store.applyFinalAnswer('native', '冲突报告');
    expect(useChatStore.getState().messages.at(-1)?.content).toBe('最终报告');
  });

  it('首块前失败不会开放分叉，只有隐藏候选时不留下空壳', () => {
    const store = useChatStore.getState();
    store.projectAssistantError('native', '首请求失败');
    store.settleAssistantMessage('native');
    const failed = useChatStore.getState().messages.at(-1);
    if (!failed) throw new Error('missing failed assistant');
    const { rerender, container } = render(<MessageBubble message={failed} onFork={vi.fn()} />);
    expect(screen.queryByRole('button', { name: '分叉会话' })).toBeNull();
    rerender(
      <MessageBubble
        message={{
          ...failed,
          content: '',
          assistantBlocks: [
            { block: block('candidate', 'final_answer', '未通过', 0), content: '未通过' },
          ],
        }}
      />
    );
    expect(container.childElementCount).toBe(0);
  });
});
