import type {
  ChatEventEnvelope,
  NativeJournalCursor,
  NativePresentationRecord,
  NativePresentationSupplement,
  NativeTurnSource,
  SavedMessage,
} from '../types/api';

export function nativeSourceKey(source: NativeTurnSource): string {
  return JSON.stringify([
    source.workspace_id,
    source.conversation_id,
    source.root_turn_id,
    source.turn_id,
    source.execution_id,
  ]);
}

export function isNativeJournalCursor(
  value: NativeJournalCursor | null | undefined
): value is NativeJournalCursor {
  return Boolean(
    value && value.stream_id && Number.isSafeInteger(value.sequence) && value.sequence > 0
  );
}

export function earliestNativeJournalCursor(
  previous: NativeJournalCursor | null | undefined,
  next: NativeJournalCursor | null | undefined
): NativeJournalCursor | null | undefined {
  if (previous === null) return null;
  if (!isNativeJournalCursor(next)) return previous;
  if (!isNativeJournalCursor(previous)) return next;
  if (previous.stream_id !== next.stream_id) return null;
  return next.sequence < previous.sequence ? next : previous;
}

/** 只从同一个已解码原envelope摘引用；不构造发布、终态或用量事件。 */
export function presentationRecordFromEnvelope(
  envelope: ChatEventEnvelope
): NativePresentationRecord | null {
  const cursor = { stream_id: envelope.stream_id, sequence: envelope.sequence };
  if (!isNativeJournalCursor(cursor)) return null;
  let payload: NativePresentationRecord['payload'];
  if (envelope.payload.source === 'agent') {
    const agent = envelope.payload.event;
    if (
      agent.conversation_id !== envelope.conversation_id ||
      agent.turn_id !== envelope.turn_id ||
      agent.message_id !== envelope.message_id
    )
      return null;
    const event = agent.payload;
    if (event.type === 'tool_call')
      payload = {
        kind: 'tool_call',
        data: {
          execution_id: agent.execution_id,
          publication_event_id: agent.event_id,
          call_id: event.data.call_id,
          name: event.data.invocation.name,
        },
      };
    else if (event.type === 'chart')
      payload = {
        kind: 'chart',
        data: {
          execution_id: agent.execution_id,
          publication_event_id: agent.event_id,
          spec: event.data.spec,
        },
      };
    else return null;
  } else if (envelope.payload.source === 'execution') {
    const event = envelope.payload.event;
    if (
      event.workspace_id !== envelope.workspace_id ||
      event.conversation_id !== envelope.conversation_id ||
      event.scope !== 'subagent' ||
      ![
        'started',
        'completed',
        'failed',
        'cancelled',
        'timed_out',
        'subagent_stream_gap',
        'isolation_observed',
      ].includes(event.event)
    )
      return null;
    payload = { kind: 'subagent', data: { event } };
  } else return null;
  return {
    cursor,
    event_id: envelope.event_id,
    content_hash: envelope.content_hash,
    timestamp: envelope.timestamp,
    scope: {
      workspace_id: envelope.workspace_id,
      conversation_id: envelope.conversation_id,
      root_turn_id: envelope.root_turn_id,
      turn_id: envelope.turn_id,
      message_id: envelope.message_id,
    },
    payload,
  };
}

export function recordBelongsToTurn(
  record: NativePresentationRecord,
  source: NativeTurnSource
): boolean {
  if (
    record.scope.workspace_id !== source.workspace_id ||
    record.scope.conversation_id !== source.conversation_id ||
    record.scope.root_turn_id !== source.root_turn_id
  )
    return false;
  if (record.payload.kind === 'subagent') {
    const event = record.payload.data.event;
    return (
      event.workspace_id === source.workspace_id &&
      event.conversation_id === source.conversation_id &&
      event.framework_event?.parent_execution_id === source.execution_id
    );
  }
  return (
    record.payload.data.execution_id === source.execution_id &&
    record.scope.turn_id === source.turn_id &&
    record.scope.message_id === source.root_turn_id
  );
}

export function uniquePresentationRecords(
  records: readonly NativePresentationRecord[]
): NativePresentationRecord[] {
  const seen = new Set<string>();
  return records.filter((record) => {
    const key = JSON.stringify([record.cursor.stream_id, record.event_id, record.content_hash]);
    if (seen.has(key)) return false;
    seen.add(key);
    return true;
  });
}

/** 一页只传一份源补充，恢复时共享给同source的所有行，不受可见窗口/页首影响。 */
export function savedPresentationBySource(
  messages: readonly SavedMessage[]
): Map<string, NativePresentationSupplement | null> {
  const sources = new Map<string, NativePresentationSupplement | null>();
  for (const message of messages) {
    const value = message.native_presentation;
    if (!value) continue;
    const key = nativeSourceKey(value.source);
    const previous = sources.get(key);
    if (previous === null) continue;
    if (
      previous &&
      JSON.stringify(previous.journal_identity) !== JSON.stringify(value.journal_identity)
    ) {
      sources.set(key, null);
    } else if (previous) {
      sources.set(key, {
        ...value,
        prefix_complete: value.prefix_complete && previous.prefix_complete,
        records: uniquePresentationRecords([...previous.records, ...value.records]),
      });
    } else sources.set(key, value);
  }
  return sources;
}
