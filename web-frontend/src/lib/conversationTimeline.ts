import { isNativeTurnSource, type ChatMessage, type NativeTurnSource } from '../types/api';

export type ConversationTimelineItem =
  | { kind: 'message'; key: string; message: ChatMessage; associationUnavailable: boolean }
  | { kind: 'turn'; key: string; source: NativeTurnSource; messages: ChatMessage[] };

/** 仅建立只读展示分组，原消息、顺序、owner 和 Fork 索引均保持不变。 */
export function conversationTimeline(
  messages: readonly ChatMessage[],
  containerKey: string
): ConversationTimelineItem[] {
  const items: ConversationTimelineItem[] = [];
  const seenTurns = new Set<string>();
  for (const message of messages) {
    const source = message.turnSource;
    if (message.role !== 'assistant' || message.internalAgent || !isNativeTurnSource(source)) {
      items.push({
        kind: 'message',
        key: message.id,
        message,
        associationUnavailable:
          message.role === 'assistant' &&
          !message.internalAgent &&
          Boolean(message.assistantBlocks?.length),
      });
      continue;
    }
    // 目标容器与源provenance分开，Fork不因此成为源execution的owner。
    const key = JSON.stringify([
      containerKey,
      source.workspace_id,
      source.conversation_id,
      source.root_turn_id,
      source.turn_id,
      source.execution_id,
    ]);
    const previous = items.at(-1);
    if (previous?.kind === 'turn' && previous.key === key) previous.messages.push(message);
    else if (seenTurns.has(key))
      items.push({ kind: 'message', key: message.id, message, associationUnavailable: true });
    else {
      seenTurns.add(key);
      items.push({ kind: 'turn', key, source, messages: [message] });
    }
  }
  return items;
}
