// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { ChatMessage, NativeTurnSource } from '../../types/api';
import { useChatStore } from '../../stores/chatStore';
import { useTaskRuntimeStore } from '../../stores/taskRuntimeStore';
import { useSubagentRunStore } from '../../stores/subagentRunStore';
import { useToolExecutionStore } from '../../stores/toolExecutionStore';
import { ConversationTimeline } from './ConversationTimeline';
import { NativeMessageTimeline } from './NativeMessageTimeline';
import { conversationTimeline } from '../../lib/conversationTimeline';

vi.hoisted(() => {
  Object.defineProperty(window, 'matchMedia', {
    configurable: true,
    value: () => ({ matches: false }),
  });
});

const source: NativeTurnSource = {
  workspace_id: 'workspace',
  conversation_id: 'conversation',
  root_turn_id: 'root',
  turn_id: 'turn',
  execution_id: 'execution',
};

function native(
  id: string,
  phase: 'reasoning' | 'commentary' | 'partial_answer' | 'final_answer',
  text: string
): ChatMessage {
  return {
    id,
    role: 'assistant',
    content: '',
    timestamp: 1,
    turnSource: source,
    assistantBlocks: [
      {
        execution_id: source.execution_id,
        publication_event_id: `event:${id}`,
        block: {
          block_id: id,
          request_id: `request:${id}`,
          sequence: 0,
          phase,
          delta: text,
          complete: true,
          provider: null,
          reasoning: null,
        },
        content: text,
      },
    ],
    executionSteps: [{ type: 'assistant', index: 0 }],
  };
}

function final(): ChatMessage {
  return {
    id: 'final',
    role: 'assistant',
    content: '最终报告',
    timestamp: 3,
    turnSource: source,
    finalAnswerConfirmed: true,
    finalAnswerExecutionId: source.execution_id,
    isStreaming: false,
  };
}

beforeEach(() => {
  useChatStore.getState().clearMessages();
  useTaskRuntimeStore.getState().reset();
  useSubagentRunStore.getState().clear();
  useToolExecutionStore.getState().clear();
});
afterEach(cleanup);

describe('可信轮次的统一过程呈现', () => {
  it('冷历史多条assistant与最终答案合为一轮，过程一次展开且原记录不变', () => {
    const thinking = native('thinking', 'reasoning', '检查实际调用路径');
    thinking.executionSteps?.push(
      ...Array.from({ length: 20 }, (_, i) => ({ type: 'tool' as const, callId: `call-${i}` }))
    );
    const messages = [
      native('first', 'commentary', '先核对配置'),
      thinking,
      native('next', 'partial_answer', '已找到配置差异'),
      final(),
    ];
    const original = JSON.stringify(messages);
    const { container } = render(
      <ConversationTimeline messages={messages} containerKey="target" />
    );
    expect(container.querySelectorAll('[aria-label="助手轮次"]')).toHaveLength(1);
    expect(screen.getByText('最终报告')).toBeDefined();
    expect(screen.queryByText('先核对配置')).toBeNull();
    fireEvent.click(screen.getByRole('button', { name: '展开执行过程' }));
    expect(screen.getByText('先核对配置')).toBeDefined();
    expect(screen.getByText('已找到配置差异')).toBeDefined();
    expect(screen.getAllByRole('button', { name: '展开中间过程' })).toHaveLength(1);
    fireEvent.click(screen.getByRole('button', { name: '展开中间过程' }));
    expect(screen.getByText('检查实际调用路径')).toBeDefined();
    expect(screen.getByText('工具记录 call-19 · 详情未恢复')).toBeDefined();
    const text = container.textContent ?? '';
    expect(text.indexOf('先核对配置')).toBeLessThan(text.indexOf('检查实际调用路径'));
    expect(text.indexOf('工具记录 call-19')).toBeLessThan(text.indexOf('已找到配置差异'));
    fireEvent.click(screen.getByRole('button', { name: '折叠执行过程' }));
    expect(screen.queryByText('先核对配置')).toBeNull();
    expect(screen.getByText('最终报告')).toBeDefined();
    expect(JSON.stringify(messages)).toBe(original);
  });

  it('运行中的两个公开正文之间只留一行；仅块完成或裸候选不折叠整轮', () => {
    const messages = [
      native('first', 'commentary', '正在检查'),
      native('one', 'reasoning', '思考一'),
      native('two', 'reasoning', '思考二'),
      native('next', 'partial_answer', '继续验证'),
      native('candidate', 'final_answer', '未发布候选'),
    ];
    render(<ConversationTimeline messages={messages} containerKey="target" />);
    expect(screen.getByText('正在检查')).toBeDefined();
    expect(screen.getByText('继续验证')).toBeDefined();
    expect(screen.getAllByRole('button', { name: '展开思考' })).toHaveLength(1);
    expect(screen.queryByRole('button', { name: '展开执行过程' })).toBeNull();
    expect(screen.queryByText('未发布候选')).toBeNull();
    fireEvent.click(screen.getByRole('button', { name: '展开思考' }));
    expect(screen.getByText('思考一')).toBeDefined();
    expect(screen.getByText('思考二')).toBeDefined();
  });

  it('补入同一过程更早的事件时保留展开、原入口和焦点', () => {
    const later = native('later', 'reasoning', '已加载的思考');
    const earlier = native('earlier', 'reasoning', '补入的早期思考');
    const { rerender } = render(<NativeMessageTimeline messages={[later]} />);
    const button = screen.getByRole('button', { name: '展开思考' });
    fireEvent.click(button);
    act(() => button.focus());
    rerender(<NativeMessageTimeline messages={[earlier, later]} />);
    expect(screen.getByRole('button', { name: '折叠思考' })).toBe(button);
    expect(document.activeElement).toBe(button);
    expect(screen.getByText('已加载的思考')).toBeDefined();
    expect(screen.getByText('补入的早期思考')).toBeDefined();
  });

  it('缺坐标、不同execution及新用户边界不靠相同正文猜组', () => {
    const a = native('a', 'commentary', '相同正文');
    const b = { ...native('b', 'commentary', '相同正文'), turnSource: undefined };
    const c = {
      ...native('c', 'commentary', '相同正文'),
      turnSource: { ...source, execution_id: 'other' },
    };
    const user: ChatMessage = { id: 'user', role: 'user', content: '下一项', timestamp: 2 };
    const projection = conversationTimeline([a, b, c, user, final()], 'target');
    expect(projection.map((item) => item.kind)).toEqual([
      'turn',
      'message',
      'turn',
      'message',
      'message',
    ]);
    render(<ConversationTimeline messages={[a, b, c, user, final()]} containerKey="target" />);
    expect(screen.getByText('最终报告')).toBeDefined();
    expect(screen.getAllByText('轮次关联未恢复')).toHaveLength(2);
  });

  it('整轮收起后显式错误和最终答案均可见', () => {
    const failed = {
      ...native('failure', 'commentary', '错误前的进度'),
      content: '[Error] 保存未确认',
      failureNotice: '保存未确认',
    };
    render(<ConversationTimeline messages={[failed, final()]} containerKey="target" />);
    expect(screen.getByText('[Error] 保存未确认')).toBeDefined();
    expect(screen.queryByText('错误前的进度')).toBeNull();
    expect(screen.getByText('最终报告')).toBeDefined();
  });
});
