// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { useChatStore } from '../../stores/chatStore';
import { useSubagentRunStore } from '../../stores/subagentRunStore';
import { useTaskRuntimeStore } from '../../stores/taskRuntimeStore';
import { useToolExecutionStore } from '../../stores/toolExecutionStore';
import type { ChatMessage, ToolExecution } from '../../types/api';
import { MessageBubble, isExecutionProcessCompleted } from './MessageBubble';

const emptyReply: ChatMessage = {
  id: 'empty-assistant',
  role: 'assistant',
  content: '',
  timestamp: 1,
  isStreaming: false,
};

beforeEach(() => {
  useChatStore.setState({ messages: [], isStreaming: false });
  useTaskRuntimeStore.getState().reset();
  useSubagentRunStore.getState().clear();
  useToolExecutionStore.getState().clear();
});
afterEach(cleanup);

describe('聊天正文与空消息投影', () => {
  it('已结束的空 canonical assistant 行不会留下机器人或消息空壳', () => {
    const { container, rerender } = render(<MessageBubble message={emptyReply} />);
    expect(container.childElementCount).toBe(0);
    rerender(<MessageBubble message={{ ...emptyReply, content: ' \n\t ' }} />);
    expect(container.childElementCount).toBe(0);
  });

  it('多条空记录之后仍保留正文与操作，不绘制机器人列', () => {
    const messages: ChatMessage[] = [
      ...Array.from({ length: 4 }, (_, index) => ({ ...emptyReply, id: `empty-${index}` })),
      { ...emptyReply, id: 'final', content: '真实结果 中文 🧭', finalAnswerConfirmed: true },
    ];
    const regenerate = vi.fn();
    const fork = vi.fn();
    const { container } = render(
      <>
        {messages.map((message) => (
          <MessageBubble
            key={message.id}
            message={message}
            onRegenerate={regenerate}
            onFork={fork}
          />
        ))}
      </>
    );
    expect(container.childElementCount).toBe(1);
    expect(container.querySelector('svg.lucide-bot')).toBeNull();
    expect(screen.getByText('真实结果 中文 🧭')).toBeDefined();
    expect(screen.getByRole('button', { name: '复制' })).toBeDefined();
    fireEvent.click(screen.getByRole('button', { name: '重新生成' }));
    fireEvent.click(screen.getByRole('button', { name: '分叉会话' }));
    expect(regenerate).toHaveBeenCalledExactlyOnceWith('final');
    expect(fork).toHaveBeenCalledExactlyOnceWith('final');
  });

  it('附件独立于正文保留，文件和图片仍可打开', () => {
    const message: ChatMessage = {
      ...emptyReply,
      attachments: [
        { name: '报告.txt', mime_type: 'text/plain', size: 200, url: '/synthetic/report.txt' },
        { name: '截图.png', mime_type: 'image/png', size: 300, url: '/synthetic/screenshot.png' },
      ],
    };
    render(<MessageBubble message={message} />);
    expect(screen.getByRole('link', { name: /报告.txt/ }).getAttribute('download')).toBe(
      '报告.txt'
    );
    expect(screen.getByRole('button', { name: '打开图片 截图.png' })).toBeDefined();
  });

  it('无正文的工具执行仍是可展开状态行并保留失败身份', () => {
    const tool: ToolExecution = {
      id: 'tool-1',
      workspace_id: 'workspace-1',
      conversation_id: 'conversation-1',
      run_id: 'run-1',
      call_id: 'call-1',
      owner: { kind: 'chat', message_id: emptyReply.id },
      name: 'read_file',
      args_preview: '只读工具',
      status: 'failed',
      started_at: 1,
      finished_at: 2,
      duration_ms: 1,
      detail_ref: '',
    };
    useToolExecutionStore.getState().replaceAll([tool]);
    render(
      <MessageBubble
        message={{ ...emptyReply, executionSteps: [{ type: 'tool', callId: tool.id }] }}
      />
    );
    fireEvent.click(screen.getByRole('button', { name: '展开执行过程' }));
    expect(screen.getByText(/1 个失败$/)).toBeDefined();
    fireEvent.click(screen.getByRole('button', { name: '展开工具执行过程' }));
    expect(screen.getByText('read_file')).toBeDefined();
    expect(screen.getByText('failed', { exact: true })).toBeDefined();
  });

  it('原生Reasoning始终从typed块显示，不需要旧字符串思考投影', () => {
    render(
      <MessageBubble
        message={{
          ...emptyReply,
          assistantBlocks: [
            {
              block: {
                block_id: 'reasoning',
                request_id: 'request-native',
                sequence: 0,
                phase: 'reasoning',
                delta: '原生推理摘要',
                complete: true,
                provider: 'fixture',
                reasoning: null,
              },
              content: '原生推理摘要',
            },
          ],
          executionSteps: [{ type: 'assistant', index: 0 }],
        }}
      />
    );
    expect(screen.queryByRole('button', { name: '展开历史执行说明' })).toBeNull();
    fireEvent.click(screen.getByRole('button', { name: '展开思考' }));
    expect(screen.getByText('原生推理摘要')).toBeDefined();
  });

  it('只有关联 Subagent 时保留其执行详情', () => {
    useChatStore.setState({ messages: [emptyReply] });
    useSubagentRunStore.getState().ingest({
      kind: 'subagent',
      workspace_id: 'workspace-1',
      conversation_id: 'conversation-1',
      subagent_run_id: 'subagent-1',
      run_id: 'run-1',
      task_id: 'task-1',
      agent: 'reviewer',
      event: 'failed',
      message_id: emptyReply.id,
      summary: '真实 Subagent 失败原因',
    });
    render(<MessageBubble message={emptyReply} />);
    fireEvent.click(screen.getByRole('button', { name: '展开执行过程' }));
    expect(screen.getByRole('button', { name: /Subagent.*reviewer.*失败/ })).toBeDefined();
    fireEvent.click(screen.getByRole('button', { name: '展开结果摘要' }));
    expect(screen.getByText('真实 Subagent 失败原因')).toBeDefined();
  });

  it('流式正文从空到内容再结算时不丢失内容或操作', () => {
    const { container, rerender } = render(
      <MessageBubble message={{ ...emptyReply, isStreaming: true }} onRegenerate={vi.fn()} />
    );
    expect(screen.queryByRole('button', { name: '重新生成' })).toBeNull();
    expect(container.childElementCount).toBe(0);
    rerender(
      <MessageBubble
        message={{ ...emptyReply, content: '流式正文 中文', isStreaming: true }}
        onRegenerate={vi.fn()}
      />
    );
    expect(screen.getByText('流式正文 中文')).toBeDefined();
    expect(container.querySelector('svg.lucide-bot')).toBeNull();
    rerender(
      <MessageBubble message={{ ...emptyReply, content: '完整结果 中文' }} onRegenerate={vi.fn()} />
    );
    expect(screen.getByText('完整结果 中文')).toBeDefined();
    expect(screen.getByRole('button', { name: '重新生成' })).toBeDefined();
  });

  it('尚未恢复详情的工具仍保留可观察的未知状态', () => {
    render(
      <MessageBubble
        message={{ ...emptyReply, executionSteps: [{ type: 'tool', callId: 'not-restored-call' }] }}
      />
    );
    fireEvent.click(screen.getByRole('button', { name: '展开执行过程' }));
    expect(screen.getByText('1 个工具 · 1 个状态未恢复')).toBeDefined();
  });

  it('无正文的内部消息标记保留，普通空行不冒充内部消息', () => {
    render(<MessageBubble message={{ ...emptyReply, internalAgent: true }} />);
    expect(screen.getByText('Agent 内部消息')).toBeDefined();
  });

  it('附件伴随空白正文时不增加空 Markdown 和复制按钮', () => {
    render(
      <MessageBubble
        message={{
          ...emptyReply,
          content: ' \n ',
          attachments: [
            {
              name: '附件.txt',
              mime_type: 'text/plain',
              size: 1,
              url: '/synthetic/attachment.txt',
            },
          ],
        }}
      />
    );
    expect(screen.getByRole('link', { name: /附件.txt/ })).toBeDefined();
    expect(screen.queryByRole('button', { name: '复制' })).toBeNull();
  });

  it('无正文但正在执行的 Subagent 仍实时可见', () => {
    useChatStore.setState({ messages: [emptyReply], isStreaming: true });
    useSubagentRunStore.getState().ingest({
      kind: 'subagent',
      workspace_id: 'workspace-1',
      conversation_id: 'conversation-1',
      subagent_run_id: 'active-subagent',
      run_id: 'run-1',
      task_id: 'task-1',
      agent: 'reviewer',
      event: 'started',
      message_id: emptyReply.id,
    });
    render(<MessageBubble message={{ ...emptyReply, isStreaming: true }} />);
    expect(screen.queryByRole('button', { name: '展开执行过程' })).toBeNull();
    expect(screen.getByRole('button', { name: /Subagent.*reviewer/ })).toBeDefined();
  });

  it('错误与内部消息保持明确，代码块与复制操作仍存在', () => {
    const { rerender } = render(
      <MessageBubble message={{ ...emptyReply, content: '[Error] 真实执行失败' }} />
    );
    expect(screen.getByText('[Error] 真实执行失败')).toBeDefined();
    rerender(
      <MessageBubble
        message={{
          ...emptyReply,
          internalAgent: true,
          content: '内部说明\n\n```rust\nlet a = 1;\n```',
        }}
        onFork={vi.fn()}
      />
    );
    expect(screen.getByText('Agent 内部消息')).toBeDefined();
    expect(screen.getByText('let a = 1;')).toBeDefined();
    expect(screen.getAllByRole('button', { name: '复制' }).length).toBe(2);
    expect(screen.queryByRole('button', { name: '分叉会话' })).toBeNull();
  });
});

it('does not mark a non-streaming tool history completed while its exact tool is still running', () => {
  const tool: ToolExecution = {
    id: 'still-running-detail',
    workspace_id: 'workspace-1',
    conversation_id: 'conversation-1',
    run_id: 'run-1',
    call_id: 'still-running-call',
    owner: { kind: 'chat', message_id: emptyReply.id },
    name: 'read_file',
    args_preview: '原始调用',
    status: 'running',
    started_at: 1,
    finished_at: null,
    duration_ms: null,
    detail_ref: '',
  };
  useToolExecutionStore.getState().replaceAll([tool]);
  render(
    <MessageBubble
      message={{ ...emptyReply, executionSteps: [{ type: 'tool', callId: tool.id }] }}
    />
  );
  expect(screen.queryByText('已完成', { exact: true })).toBeNull();
  expect(screen.queryByRole('button', { name: '展开执行过程' })).toBeNull();
  expect(screen.getByText('正在执行 1 个工具')).toBeDefined();
});

it('retains the exact TaskRun terminal authority over delayed tool presentation', () => {
  expect(isExecutionProcessCompleted(false, true, 'completed', [], true)).toBe(true);
  expect(isExecutionProcessCompleted(false, true, 'failed', [], true)).toBe(true);
  expect(isExecutionProcessCompleted(false, false, null, [], true)).toBe(false);
});

it('keeps a known tool failure visible while the settled process is collapsed', () => {
  const tool: ToolExecution = {
    id: 'failed-history-detail',
    workspace_id: 'workspace-1',
    conversation_id: 'conversation-1',
    run_id: 'run-1',
    call_id: 'failed-history-call',
    owner: { kind: 'chat', message_id: emptyReply.id },
    name: 'read_file',
    args_preview: '有错误的调用',
    status: 'failed',
    started_at: 1,
    finished_at: 2,
    duration_ms: 1,
    detail_ref: '',
  };
  useToolExecutionStore.getState().replaceAll([tool]);
  const { container } = render(
    <MessageBubble
      message={{ ...emptyReply, executionSteps: [{ type: 'tool', callId: tool.id }] }}
    />
  );
  expect(screen.getByText('1 个工具失败')).toBeDefined();
  expect(screen.getByRole('button', { name: '展开执行过程' }).getAttribute('aria-expanded')).toBe(
    'false'
  );
  expect(container.querySelector('.lucide-check')).toBeNull();
  expect(useToolExecutionStore.getState().tools[tool.id]?.status).toBe('failed');
});
