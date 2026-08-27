import { beforeEach, describe, expect, it } from 'vitest';
import type { ChatEventEnvelope } from '../types/api';
import {
  ChatEventSequencer,
  forgetChatEventStream,
  resetChatEventCursorsForTest,
} from './chatEventSequencer';

const envelope = (sequence: number): ChatEventEnvelope => ({
  schema_version: 2,
  workspace_id: 'workspace-1',
  event_id: `event-${sequence}`,
  content_hash: `hash-${sequence}`,
  sequence,
  stream_id: 'conversation:one',
  conversation_id: 'one',
  root_turn_id: 'turn-1',
  turn_id: 'turn-1',
  message_id: 'turn-1',
  timestamp: '2026-08-18T00:00:00Z',
  payload: { source: 'turn_status', event: { status: 'running' } },
});

const turnEnvelope = (sequence: number, turnId: string): ChatEventEnvelope => ({
  ...envelope(sequence),
  event_id: `event-${turnId}-${sequence}`,
  content_hash: `hash-${turnId}-${sequence}`,
  turn_id: turnId,
  root_turn_id: turnId,
  message_id: turnId,
});

const lifecycleEnvelope = (sequence: number, phase: 'persisted' | 'attempt_started') =>
  ({
    ...turnEnvelope(sequence, `input-${phase}`),
    payload: { source: 'input_lifecycle', event: { phase } },
  }) as ChatEventEnvelope;

describe('ChatEventSequencer', () => {
  beforeEach(resetChatEventCursorsForTest);

  it('orders a live event that races ahead of replay and deduplicates it', () => {
    const sequencer = new ChatEventSequencer();
    const applied: number[] = [];
    sequencer.ingest(envelope(3), (event) => applied.push(event.sequence));
    sequencer.ingestReplay(
      {
        events: [envelope(1), envelope(2), envelope(3)],
        retained_earliest_cursor: 1,
        returned_earliest_cursor: 1,
        latest_cursor: 3,
        truncated: false,
      },
      (event) => applied.push(event.sequence)
    );
    expect(applied).toEqual([1, 2, 3]);
  });

  it('rebases only when the backend explicitly reports a retention gap', () => {
    const sequencer = new ChatEventSequencer();
    const applied: number[] = [];
    sequencer.ingestReplay(
      {
        events: [envelope(8), envelope(9)],
        retained_earliest_cursor: 5,
        returned_earliest_cursor: 8,
        latest_cursor: 9,
        truncated: true,
      },
      (event) => applied.push(event.sequence)
    );
    expect(applied).toEqual([8, 9]);
    expect(sequencer.cursor('conversation:one')).toBe(9);
  });

  it('drops an old cursor and pending gap when a deleted stream ID is recreated', () => {
    const sequencer = new ChatEventSequencer();
    const applied: number[] = [];
    const apply = (event: ChatEventEnvelope) => applied.push(event.sequence);

    sequencer.ingest(envelope(1), apply);
    sequencer.ingest(envelope(3), apply);
    forgetChatEventStream('conversation:one');
    sequencer.ingest(envelope(1), apply);
    sequencer.ingest(envelope(2), apply);

    expect(applied).toEqual([1, 1, 2]);
    expect(sequencer.cursor('conversation:one')).toBe(2);
  });

  it('advances the cursor without projecting an observed older turn over the latest turn', () => {
    const sequencer = new ChatEventSequencer();
    const projected: string[] = [];
    const apply = (event: ChatEventEnvelope) => projected.push(event.turn_id);

    sequencer.ingest(turnEnvelope(1, 'turn-a'), apply);
    sequencer.ingest(turnEnvelope(2, 'turn-b'), apply);
    sequencer.ingest(turnEnvelope(3, 'turn-a'), apply);
    sequencer.ingest(turnEnvelope(4, 'turn-b'), apply);

    expect(projected).toEqual(['turn-a', 'turn-b', 'turn-b']);
    expect(sequencer.cursor('conversation:one')).toBe(4);
  });

  it('applies lifecycle facts continuously without changing latest-turn selection', () => {
    const sequencer = new ChatEventSequencer();
    const projected: Array<[number, string]> = [];
    const apply = (event: ChatEventEnvelope) => projected.push([event.sequence, event.turn_id]);

    sequencer.ingest(lifecycleEnvelope(1, 'persisted'), apply);
    sequencer.ingest(lifecycleEnvelope(2, 'attempt_started'), apply);
    sequencer.ingest(turnEnvelope(3, 'turn-live'), apply);
    sequencer.ingest(lifecycleEnvelope(4, 'attempt_started'), apply);
    sequencer.ingest(turnEnvelope(5, 'turn-live'), apply);

    expect(projected).toEqual([
      [1, 'input-persisted'],
      [2, 'input-attempt_started'],
      [3, 'turn-live'],
      [4, 'input-attempt_started'],
      [5, 'turn-live'],
    ]);
    expect(sequencer.cursor('conversation:one')).toBe(5);
  });
});
