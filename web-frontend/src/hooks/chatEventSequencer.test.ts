import { beforeEach, describe, expect, it } from 'vitest';
import type { ChatEventEnvelope } from '../types/api';
import {
  ChatEventSequencer,
  forgetChatEventStream,
  recordTerminalStatusForTurn,
  resetChatEventCursorsForTest,
  terminalStatusForTurn,
} from './chatEventSequencer';

const envelope = (sequence: number): ChatEventEnvelope => ({
  schema_version: 1,
  event_id: `event-${sequence}`,
  content_hash: `hash-${sequence}`,
  sequence,
  stream_id: 'conversation:one',
  conversation_id: 'one',
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
  message_id: turnId,
});

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

  it('preserves terminal monotonicity across remount and clears it with stream deletion', () => {
    expect(recordTerminalStatusForTurn('conversation:one', 'turn-1', 'cancelled')).toBe(true);

    const remounted = new ChatEventSequencer();
    expect(remounted.cursor('conversation:one')).toBe(0);
    expect(terminalStatusForTurn('conversation:one', 'turn-1')).toBe('cancelled');
    expect(recordTerminalStatusForTurn('conversation:one', 'turn-1', 'completed')).toBe(false);

    forgetChatEventStream('conversation:one');
    expect(terminalStatusForTurn('conversation:one', 'turn-1')).toBeNull();
  });

  it('does not let an older turn erase a newer turn terminal', () => {
    expect(recordTerminalStatusForTurn('conversation:one', 'turn-b', 'failed')).toBe(true);
    expect(recordTerminalStatusForTurn('conversation:one', 'turn-a', 'completed')).toBe(true);

    expect(terminalStatusForTurn('conversation:one', 'turn-b')).toBe('failed');
    expect(recordTerminalStatusForTurn('conversation:one', 'turn-b', 'completed')).toBe(false);
    expect(terminalStatusForTurn('conversation:one', 'turn-a')).toBe('completed');
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
});
