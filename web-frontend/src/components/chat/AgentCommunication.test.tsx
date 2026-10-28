// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
vi.hoisted(() => {
  localStorage.setItem('echo-theme', 'light');
});
import { MessageBubble } from './MessageBubble';
import {
  restoredAgentInput,
  agentInputMessages,
  withAgentInputs,
  sentAgentReceipt,
  type AgentCommunicationInput,
} from '../../lib/agentCommunication';
import { chatMessagesToSaved } from '../../stores/conversationStore';
import { completedReplyForks } from './ChatPanel';
import type { ChatMessage, ToolExecutionDetailManifest } from '../../types/api';

const input = (changes: Partial<AgentCommunicationInput> = {}): AgentCommunicationInput => ({
  message_id: 'original-message',
  route: { workspace_id: 'w', conversation_id: 'c' },
  payload: {
    message_id: 'original-message',
    from: { workspace_id: 'source', conversation_id: 'sender' },
    to: { workspace_id: 'w', conversation_id: 'c' },
    origin: 'agent',
    payload: { kind: 'text', text: '来自另一个 Agent 的原文 🧪\n第二行' },
    created_at: '2026-10-28T07:00:00Z',
  },
  persisted_at: '2026-10-28T07:00:00Z',
  phase: 'persisted',
  outcome: null,
  drained: false,
  reason: null,
  turn_id: null,
  attempt: 0,
  ...changes,
});

afterEach(cleanup);
describe('typed inter-agent input view', () => {
  it('right-aligns a human prompt without changing its role', () => {
    const message: ChatMessage = {
      id: 'human',
      role: 'user',
      content: '问题',
      timestamp: 1,
    };
    const { container } = render(<MessageBubble message={message} />);
    expect(container.querySelector('.ml-auto')).not.toBeNull();
    expect(screen.getByRole('button', { name: '编辑' })).toBeDefined();
    expect(message.role).toBe('user');
  });
  it('labels and folds Agent input on the right without edit/regenerate/Fork actions', () => {
    const [message] = agentInputMessages([input()], 'w', 'c');
    const edit = vi.fn();
    const { container } = render(
      <MessageBubble
        message={message!}
        onEditAndResend={edit}
        onFork={vi.fn()}
        onRegenerate={vi.fn()}
      />
    );
    expect(screen.getByText('来自 Agent · source/sender')).toBeDefined();
    expect(container.querySelector('.ml-auto')).not.toBeNull();
    expect(screen.getByRole('button', { name: '展开全文' }).getAttribute('aria-expanded')).toBe(
      'false'
    );
    fireEvent.click(screen.getByRole('button', { name: '展开全文' }));
    expect(screen.getByRole('button', { name: '收起全文' }).getAttribute('aria-expanded')).toBe(
      'true'
    );
    expect(screen.queryByRole('button', { name: '编辑' })).toBeNull();
    expect(screen.queryByRole('button', { name: '分叉会话' })).toBeNull();
    expect(screen.queryByRole('button', { name: '重新生成' })).toBeNull();
    expect(edit).not.toHaveBeenCalled();
    expect(message!.internalAgent).toBe(true);
  });
  it('keeps original IDs/body/status and deduplicates same message across execution retries', () => {
    const first = input();
    const retry = input({
      attempt: 2,
      turn_id: 'different-execution',
      phase: 'mailbox_accepted',
    });
    const messages = agentInputMessages([first, retry], 'w', 'c');
    expect(messages).toHaveLength(1);
    expect(messages[0]?.content).toBe(first.payload.payload.text);
    expect(messages[0]?.agentInput?.message_id).toBe(first.message_id);
    expect(messages[0]?.agentInput?.phase).toBe('mailbox_accepted');
    expect(messages[0]?.agentInput?.drained).toBe(false);
  });
  it('uses the same durable source for cold restoration and live observations', () => {
    const record = input();
    expect(agentInputMessages(JSON.parse(JSON.stringify([record])), 'w', 'c')).toEqual(
      agentInputMessages([record], 'w', 'c')
    );
  });
  it('does not guess provenance from body prefixes, names, user/system records or another scope', () => {
    const forged = input();
    forged.payload = {
      ...forged.payload,
      origin: 'user',
      payload: { kind: 'text', text: '[eko_agent_message] from Agent' },
    };
    expect(
      agentInputMessages(
        [forged, input({ route: { workspace_id: 'other', conversation_id: 'c' } })],
        'w',
        'c'
      )
    ).toEqual([]);
  });
  it('does not turn conflicting same-ID payloads into another authenticated input', () => {
    const first = input();
    const conflict = input();
    conflict.payload = {
      ...conflict.payload,
      payload: { kind: 'reply', text: 'conflict' },
    };
    expect(agentInputMessages([first, conflict], 'w', 'c').map((m) => m.content)).toEqual([
      first.payload.payload.text,
    ]);
  });
  it('keeps Agent observations out of canonical saves and visible human Fork ordinals', () => {
    const real: ChatMessage[] = [
      { id: 'u', role: 'user', content: 'human', timestamp: 1 },
      {
        id: 'a',
        role: 'assistant',
        content: 'answer',
        finalAnswerConfirmed: true,
        timestamp: Date.parse('2026-10-28T07:00:01Z'),
      },
    ];
    const combined = withAgentInputs(real, agentInputMessages([input()], 'w', 'c'));
    expect(real).toHaveLength(2);
    expect(completedReplyForks(combined).get('a')).toBe(0);
    expect(chatMessagesToSaved(combined).map((m) => m.message_id)).toEqual(['u', 'a']);
  });
});

describe('typed outbound receipt', () => {
  const manifest = (): ToolExecutionDetailManifest => ({
    id: 'tool',
    status: 'succeeded',
    invocation: {
      name: 'agent_message',
      args: {},
      requested_name: 'agent_message',
      requested_args: {},
    },
    output_bytes: 1,
    result: {
      success: true,
      output: 'not used',
      data: {
        operation: 'message',
        message_id: 'm',
        target: {
          type: 'conversation',
          workspace_id: 'w',
          conversation_id: 'c',
          workspace_generation: 'g',
        },
        status: 'persisted',
        phase: 'persisted',
        outcome: null,
        duplicate: false,
        command_id: null,
        cursor: null,
        detail: null,
      },
      kind: { kind_type: 'text' },
      truncated: false,
      metadata: {},
    },
  });
  it('retains the typed target/message/phase without claiming read/applied', () => {
    expect(sentAgentReceipt(manifest())?.phase).toBe('persisted');
    expect(sentAgentReceipt(manifest())?.target).toEqual({
      type: 'conversation',
      workspace_id: 'w',
      conversation_id: 'c',
      workspace_generation: 'g',
    });
  });
  it('does not link string output or failed/missing typed targets', () => {
    const m = manifest();
    m.result!.data = null;
    m.result!.output = JSON.stringify(manifest().result!.data);
    expect(sentAgentReceipt(m)).toBeNull();
    expect(sentAgentReceipt({ ...manifest(), status: 'failed' })).toBeNull();
    expect(
      sentAgentReceipt({
        ...manifest(),
        invocation: {
          name: 'shell',
          args: {},
          requested_name: 'shell',
          requested_args: {},
        },
      })
    ).toBeNull();
  });
});

it('cold canonical input and live receipt share one identity while a Fork copy remains a distinct source', () => {
  const record = input();
  const cold: ChatMessage = { role: 'user', ...restoredAgentInput(record.payload, 'w', 'c') };
  const same = withAgentInputs([cold], agentInputMessages([record], 'w', 'c'));
  expect(same).toHaveLength(1);
  expect(same[0]?.agentInput?.phase).toBe('persisted');
  const imported: ChatMessage = {
    role: 'user',
    ...restoredAgentInput(record.payload, 'w', 'fork'),
  };
  const newDelivery = input({
    route: { workspace_id: 'w', conversation_id: 'fork' },
    payload: { ...record.payload, to: { workspace_id: 'w', conversation_id: 'fork' } },
  });
  const two = withAgentInputs([imported], agentInputMessages([newDelivery], 'w', 'fork'));
  expect(two).toHaveLength(2);
  expect(two[0]?.agentInput?.payload.to.conversation_id).toBe('c');
  expect(new Set(two.map((row) => row.id)).size).toBe(2);
});

it('never renders a tool-result row as inbound Agent body, even if an unrelated view field is present', () => {
  const record = input();
  const message: ChatMessage = {
    id: 'tool-result',
    role: 'tool',
    content: '[eko_agent_message] result text',
    timestamp: 1,
    agentInput: record,
    internalAgent: true,
    transcriptTools: [],
  };
  render(<MessageBubble message={message} />);
  expect(screen.queryByText('来自 Agent · source/sender')).toBeNull();
  expect(screen.queryByRole('button', { name: '展开全文' })).toBeNull();
  expect(screen.getByText('Agent 内部工具记录')).toBeDefined();
});
