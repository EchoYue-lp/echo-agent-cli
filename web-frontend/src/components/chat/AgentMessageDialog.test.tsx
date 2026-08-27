// @vitest-environment jsdom
import { fireEvent, render, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';

const mocks = vi.hoisted(() => ({
  list: vi.fn(),
  send: vi.fn(),
  status: vi.fn(),
  listGroups: vi.fn(),
  createGroup: vi.fn(),
  updateGroup: vi.fn(),
  deleteGroup: vi.fn(),
  toast: vi.fn(),
}));

vi.mock('../../api/endpoints', () => ({
  agentApi: {
    list: mocks.list,
    send: mocks.send,
    status: mocks.status,
    listGroups: mocks.listGroups,
    createGroup: mocks.createGroup,
    updateGroup: mocks.updateGroup,
    deleteGroup: mocks.deleteGroup,
  },
}));

vi.mock('../../stores/workspaceStore', () => ({
  useWorkspaceStore: (selector: (state: unknown) => unknown) =>
    selector({ current: { id: 'source-workspace', name: 'Source' } }),
}));

vi.mock('../../stores/conversationStore', () => ({
  useConversationStore: (selector: (state: unknown) => unknown) =>
    selector({ activeId: 'source-conversation' }),
}));

vi.mock('../../stores/toastStore', () => ({
  useToastStore: {
    getState: () => ({ addToast: mocks.toast }),
  },
}));

import { AgentMessageDialog } from './AgentMessageDialog';

describe('AgentMessageDialog', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mocks.list.mockResolvedValue({
      count: 2,
      endpoints: [
        {
          address: {
            workspace_id: 'source-workspace',
            conversation_id: 'source-conversation',
          },
          workspace_name: 'Source',
          conversation_title: 'Current',
          updated_at: '2026-08-21T00:00:00Z',
        },
        {
          address: {
            workspace_id: 'target-workspace',
            conversation_id: 'target-conversation',
          },
          workspace_name: 'Target',
          conversation_title: 'Research Agent',
          updated_at: '2026-08-21T00:00:00Z',
        },
      ],
    });
    mocks.status.mockResolvedValue({ records: [], count: 0 });
    mocks.listGroups.mockResolvedValue({ groups: [], count: 0 });
    mocks.send.mockResolvedValue({
      success: true,
      receipt: {
        message_id: 'message-1',
        target: {
          workspace_id: 'target-workspace',
          conversation_id: 'target-conversation',
        },
        phase: 'persisted',
        outcome: null,
        drained: false,
        reason: null,
        persisted_at: '2026-08-21T00:00:00Z',
        duplicate: false,
        durability: { status: 'confirmed' },
      },
    });
    mocks.createGroup.mockResolvedValue({
      success: true,
      group: {
        group_id: 'group-1',
        name: '发布组',
        leader: {
          workspace_id: 'source-workspace',
          conversation_id: 'source-conversation',
        },
        members: [
          {
            address: {
              workspace_id: 'target-workspace',
              conversation_id: 'target-conversation',
            },
            subagent_role: 'explorer',
            label: null,
          },
        ],
        created_at: '2026-08-21T00:00:00Z',
        updated_at: '2026-08-21T00:00:00Z',
      },
    });
  });

  it('discovers other conversations, sends through the Agent API, and refreshes receipts', async () => {
    const { getAllByText, getByRole, queryByText } = render(
      <AgentMessageDialog isOpen onClose={vi.fn()} />
    );

    await waitFor(() => expect(getAllByText('Research Agent')).toHaveLength(2));
    expect(queryByText('Current')).toBeNull();

    fireEvent.change(getByRole('textbox', { name: 'Agent 消息内容' }), {
      target: { value: '请检查最新结果' },
    });
    fireEvent.click(getByRole('button', { name: '发送' }));

    await waitFor(() =>
      expect(mocks.send).toHaveBeenCalledWith({
        toWorkspaceId: 'target-workspace',
        toConversationId: 'target-conversation',
        text: '请检查最新结果',
        fromWorkspaceId: 'source-workspace',
        fromConversationId: 'source-conversation',
      })
    );
    expect(mocks.status).toHaveBeenCalledWith('target-workspace', 'target-conversation');
    expect(mocks.toast).toHaveBeenCalledWith('success', '消息已排队：message-1');
  });

  it('creates a persistent group from selectable Agent conversations', async () => {
    const { getByRole } = render(<AgentMessageDialog isOpen onClose={vi.fn()} />);

    await waitFor(() => expect(mocks.list).toHaveBeenCalled());
    fireEvent.click(getByRole('tab', { name: 'Agent 组' }));
    await waitFor(() => expect(mocks.listGroups).toHaveBeenCalled());

    fireEvent.change(getByRole('textbox', { name: 'Agent 组名' }), {
      target: { value: '发布组' },
    });
    fireEvent.click(getByRole('button', { name: '保存' }));

    await waitFor(() =>
      expect(mocks.createGroup).toHaveBeenCalledWith({
        name: '发布组',
        leader: {
          workspace_id: 'source-workspace',
          conversation_id: 'source-conversation',
        },
        members: [
          {
            address: {
              workspace_id: 'target-workspace',
              conversation_id: 'target-conversation',
            },
            subagent_role: 'explorer',
            label: null,
          },
        ],
      })
    );
  });

  it('renders only canonical delivery phases and typed terminal outcomes', async () => {
    const base = {
      message_id: 'message-phase',
      target: {
        workspace_id: 'target-workspace',
        conversation_id: 'target-conversation',
      },
      persisted_at: '2026-08-21T00:00:00Z',
      attempt_id: 'attempt-1',
      attempt: 1,
      claimed_at: '2026-08-21T00:00:01Z',
      mailbox_accepted_at: null,
      drained_at: null,
      turn_settled_at: null,
      turn_id: 'turn-1',
      reply_message_id: null,
      next_attempt_at: null,
      reason: null,
    };
    mocks.status.mockResolvedValue({
      count: 2,
      records: [
        {
          ...base,
          phase: 'mailbox_accepted',
          outcome: null,
          drained: false,
          mailbox_accepted_at: '2026-08-21T00:00:02Z',
        },
        {
          ...base,
          message_id: 'message-terminal',
          phase: 'turn_settled',
          outcome: 'cancelled',
          drained: true,
          mailbox_accepted_at: '2026-08-21T00:00:02Z',
          drained_at: '2026-08-21T00:00:03Z',
          turn_settled_at: '2026-08-21T00:00:04Z',
          reason: 'cancelled by owner',
        },
      ],
    });

    const { findByText, queryByText } = render(<AgentMessageDialog isOpen onClose={vi.fn()} />);

    expect(await findByText('邮箱已接收')).toBeTruthy();
    expect(await findByText('已取消')).toBeTruthy();
    expect(await findByText('cancelled by owner')).toBeTruthy();
    expect(queryByText('注入已开始')).toBeNull();
    expect(queryByText('已注入当前任务')).toBeNull();
    expect(queryByText('已送达')).toBeNull();
  });
});
