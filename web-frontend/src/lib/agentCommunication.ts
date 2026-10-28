import type { AgentAddress, AgentControlReceipt, ConversationTarget } from '../generated';
import type { ChatMessage, ToolExecutionDetailManifest } from '../types/api';

/** Read projection of the existing durable AgentRouter record, never an instruction authority. */
export interface AgentCommunicationInput {
  message_id: string;
  route: AgentAddress;
  payload: {
    message_id: string;
    from: AgentAddress | null;
    to: AgentAddress;
    origin: 'user' | 'agent' | 'system';
    payload: { kind: 'text' | 'reply'; text: string };
    created_at: string;
  };
  persisted_at: string;
  phase: string;
  outcome: string | null;
  drained: boolean;
  reason: string | null;
  turn_id: string | null;
  attempt: number;
}

export function agentInputKey(record: AgentCommunicationInput): string {
  return JSON.stringify([
    record.route.workspace_id,
    record.route.conversation_id,
    record.message_id,
  ]);
}

/** The ledger supplies origin and identity. Body prefixes and tool output text are never inspected. */
export function agentInputMessages(
  records: readonly AgentCommunicationInput[],
  workspaceId: string,
  conversationId: string
): ChatMessage[] {
  const unique = new Map<string, AgentCommunicationInput>();
  for (const record of records) {
    if (
      record.payload.origin !== 'agent' ||
      !record.payload.from ||
      !record.message_id ||
      record.message_id !== record.payload.message_id ||
      record.route.workspace_id !== workspaceId ||
      record.route.conversation_id !== conversationId ||
      record.payload.to.workspace_id !== workspaceId ||
      record.payload.to.conversation_id !== conversationId
    )
      continue;
    const key = agentInputKey(record);
    const existing = unique.get(key);
    // A conflicting source is not silently replaced by an arrival from another execution.
    if (existing && JSON.stringify(existing.payload) !== JSON.stringify(record.payload)) continue;
    unique.set(key, record);
  }
  return [...unique.values()]
    .sort((a, b) => a.persisted_at.localeCompare(b.persisted_at))
    .map((record) => ({
      id: `agent-input:${agentInputKey(record)}`,
      role: 'user',
      internalAgent: true,
      agentInput: record,
      content: record.payload.payload.text,
      timestamp: Date.parse(record.persisted_at),
      isStreaming: false,
    }));
}

/** Merge view rows only. Canonical messages, user ordinals and assistant ownership stay unchanged. */
export function withAgentInputs(
  messages: readonly ChatMessage[],
  inputs: readonly ChatMessage[]
): ChatMessage[] {
  const result = [...messages];
  for (const input of inputs) {
    const existing = result.findIndex((message) => message.id === input.id);
    if (existing >= 0) {
      const previous = result.at(existing);
      if (
        previous?.agentInput &&
        JSON.stringify(previous.agentInput.payload) === JSON.stringify(input.agentInput?.payload)
      )
        result.splice(existing, 1, {
          ...previous,
          agentInput: input.agentInput,
        });
      continue;
    }
    const at = result.findIndex(
      (message) => Number.isFinite(message.timestamp) && message.timestamp > input.timestamp
    );
    if (at < 0) result.push(input);
    else result.splice(at, 0, input);
  }
  return result;
}

/** Only the tool's typed result.data can link a target; output/preview text cannot. */
export function sentAgentReceipt(
  manifest: ToolExecutionDetailManifest | null
): AgentControlReceipt | null {
  if (!manifest || !['agent_message', 'agent_followup'].includes(manifest.invocation.name))
    return null;
  if (manifest.status !== 'succeeded' || !manifest.result?.success) return null;
  const value = manifest.result.data;
  if (!value || typeof value !== 'object') return null;
  const receipt = value as Partial<AgentControlReceipt>;
  if (
    !['message', 'followup'].includes(receipt.operation ?? '') ||
    !receipt.message_id ||
    receipt.target?.type !== 'conversation' ||
    !receipt.target.workspace_id ||
    !receipt.target.conversation_id ||
    typeof receipt.phase !== 'string'
  )
    return null;
  return receipt as AgentControlReceipt;
}

export function receiptConversation(receipt: AgentControlReceipt): ConversationTarget | null {
  return receipt.target.type === 'conversation' ? receipt.target : null;
}

/** A canonical ResourceLink snapshot survives inbox retention and copies without becoming a new delivery. */
export function restoredAgentInput(
  payload: AgentCommunicationInput['payload'],
  workspaceId: string,
  conversationId: string
): Pick<ChatMessage, 'id' | 'content' | 'internalAgent' | 'agentInput' | 'timestamp'> {
  const record: AgentCommunicationInput = {
    message_id: payload.message_id,
    route: payload.to,
    payload,
    persisted_at: payload.created_at,
    phase: '历史来源；当前投递阶段未读取',
    outcome: null,
    drained: false,
    reason: null,
    turn_id: null,
    attempt: 0,
  };
  return {
    id:
      payload.to.workspace_id === workspaceId && payload.to.conversation_id === conversationId
        ? `agent-input:${JSON.stringify([workspaceId, conversationId, payload.message_id])}`
        : `agent-input-copy:${JSON.stringify([workspaceId, conversationId, payload.to, payload.message_id])}`,
    content: payload.payload.text,
    internalAgent: true,
    agentInput: record,
    timestamp: Date.parse(payload.created_at),
  };
}
