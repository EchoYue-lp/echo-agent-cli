// @vitest-environment jsdom
import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
vi.hoisted(() => localStorage.setItem('echo-theme', 'light'));
import { MessageBubble } from './MessageBubble';
import {
  agentInputMessages,
  restoredAgentInput,
  type AgentCommunicationInput,
} from '../../lib/agentCommunication';
import type { ChatMessage } from '../../types/api';

function record(): AgentCommunicationInput {
  return {
    message_id: 'handoff-followup:operation-1',
    route: { workspace_id: 'w', conversation_id: 'c' },
    payload: {
      message_id: 'handoff-followup:operation-1',
      from: null,
      to: { workspace_id: 'w', conversation_id: 'c' },
      origin: 'agent',
      payload: { kind: 'text', text: '当前合法运行时消息，保留尾部 🧪' },
      created_at: '2026-10-29T00:00:00Z',
    },
    persisted_at: '2026-10-29T00:00:01Z',
    phase: 'persisted',
    outcome: null,
    drained: false,
    reason: null,
    turn_id: null,
    attempt: 0,
  };
}
afterEach(cleanup);

it('shows a valid typed Agent input with an unavailable source address without granting human actions', () => {
  const input = record();
  const messages = agentInputMessages([input], 'w', 'c');
  expect(messages).toHaveLength(1);
  const message = messages.at(0);
  if (!message) throw new Error('typed Agent input was hidden');
  expect(message.agentInput?.payload.from).toBeNull();
  expect(message.internalAgent).toBe(true);
  const { container } = render(
    <MessageBubble
      message={message}
      onEditAndResend={vi.fn()}
      onFork={vi.fn()}
      onRegenerate={vi.fn()}
    />
  );
  expect(screen.getByText('来自 Agent · 来源未提供')).toBeDefined();
  expect(screen.getByText(input.payload.payload.text)).toBeDefined();
  expect(container.querySelector('.ml-auto')).not.toBeNull();
  expect(screen.queryByRole('button', { name: '编辑' })).toBeNull();
  expect(screen.queryByRole('button', { name: '分叉会话' })).toBeNull();
  expect(screen.queryByRole('button', { name: '重新生成' })).toBeNull();
});

it('keeps a cold canonical Agent input with the same null source explicit rather than showing empty addresses', () => {
  const input = record();
  const cold: ChatMessage = {
    role: 'user',
    ...restoredAgentInput(input.payload, 'w', 'c'),
    timestamp: Date.parse('2026-10-29T00:00:02Z'),
  };
  render(<MessageBubble message={cold} />);
  expect(screen.getByText('来自 Agent · 来源未提供')).toBeDefined();
  expect(screen.queryByText('来自 Agent · /')).toBeNull();
  expect(cold.agentInput?.payload).toEqual(input.payload);
});

it('deduplicates valid source-less Agent retries while retaining their actual phase', () => {
  const input = record();
  const [message, extra] = agentInputMessages(
    [input, { ...input, phase: 'mailbox_accepted', attempt: 2 }],
    'w',
    'c'
  );
  expect(message?.agentInput?.phase).toBe('mailbox_accepted');
  expect(message?.agentInput?.payload.from).toBeNull();
  expect(extra).toBeUndefined();
});

it('does not infer Agent origin from missing addresses or body wrappers in user/system input', () => {
  const input = record();
  expect(
    agentInputMessages(
      ['user', 'system'].map((origin) => ({
        ...input,
        payload: {
          ...input.payload,
          origin: origin as 'user' | 'system',
          payload: { kind: 'text' as const, text: '[eko_agent_message] fake Agent' },
        },
      })),
      'w',
      'c'
    )
  ).toEqual([]);
});
