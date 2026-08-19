import type { ChatEventEnvelope, ChatEventReplay } from '../types/api';

// Zustand survives a hook remount in the same WebView, so the applied cursor
// must survive as well. Otherwise replay can apply the same material event twice.
const appliedCursorByStream = new Map<string, number>();
const generationByStream = new Map<string, number>();
type TerminalChatStatus = 'completed' | 'failed' | 'cancelled';
const MAX_TERMINAL_TURNS_PER_STREAM = 64;
const terminalByStream = new Map<string, Map<string, TerminalChatStatus>>();
const MAX_OBSERVED_TURNS_PER_STREAM = 256;
const latestTurnByStream = new Map<string, string>();
const observedTurnsByStream = new Map<string, Map<string, true>>();

export function terminalStatusForTurn(streamId: string, turnId: string): TerminalChatStatus | null {
  return terminalByStream.get(streamId)?.get(turnId) ?? null;
}

export function recordTerminalStatusForTurn(
  streamId: string,
  turnId: string,
  status: TerminalChatStatus
): boolean {
  const current = terminalStatusForTurn(streamId, turnId);
  if (current) return current === status;
  const terminals = terminalByStream.get(streamId) ?? new Map<string, TerminalChatStatus>();
  terminals.set(turnId, status);
  while (terminals.size > MAX_TERMINAL_TURNS_PER_STREAM) {
    const oldestTurnId = terminals.keys().next().value;
    if (typeof oldestTurnId !== 'string') break;
    terminals.delete(oldestTurnId);
  }
  terminalByStream.set(streamId, terminals);
  return true;
}

export class ChatEventSequencer {
  private pendingByStream = new Map<string, Map<number, ChatEventEnvelope>>();
  private observedGenerationByStream = new Map<string, number>();

  cursor(streamId: string): number {
    this.synchronizeGeneration(streamId);
    return appliedCursorByStream.get(streamId) ?? 0;
  }

  ingest(envelope: ChatEventEnvelope, apply: (event: ChatEventEnvelope) => void): void {
    const cursor = this.cursor(envelope.stream_id);
    if (envelope.sequence <= cursor) return;
    const pending = this.pendingByStream.get(envelope.stream_id) ?? new Map();
    pending.set(envelope.sequence, envelope);
    this.pendingByStream.set(envelope.stream_id, pending);
    this.drain(envelope.stream_id, apply);
  }

  ingestReplay(replay: ChatEventReplay, apply: (event: ChatEventEnvelope) => void): void {
    const first = replay.events[0];
    if (
      replay.truncated &&
      first &&
      replay.returned_earliest_cursor === first.sequence &&
      first.sequence > this.cursor(first.stream_id) + 1
    ) {
      appliedCursorByStream.set(first.stream_id, first.sequence - 1);
    }
    for (const envelope of replay.events) {
      this.ingest(envelope, apply);
    }
  }

  private drain(streamId: string, apply: (event: ChatEventEnvelope) => void): void {
    const pending = this.pendingByStream.get(streamId);
    if (!pending) return;
    let next = this.cursor(streamId) + 1;
    while (pending.has(next)) {
      const envelope = pending.get(next);
      pending.delete(next);
      if (envelope) {
        if (shouldProjectTurn(envelope.stream_id, envelope.turn_id)) {
          apply(envelope);
        }
        appliedCursorByStream.set(streamId, next);
      }
      next += 1;
    }
    if (pending.size === 0) this.pendingByStream.delete(streamId);
  }

  private synchronizeGeneration(streamId: string): void {
    const generation = generationByStream.get(streamId) ?? 0;
    if (this.observedGenerationByStream.get(streamId) === generation) return;
    this.pendingByStream.delete(streamId);
    this.observedGenerationByStream.set(streamId, generation);
  }
}

function shouldProjectTurn(streamId: string, turnId: string): boolean {
  if (latestTurnByStream.get(streamId) === turnId) return true;
  const observed = observedTurnsByStream.get(streamId) ?? new Map<string, true>();
  if (observed.has(turnId)) return false;

  observed.set(turnId, true);
  while (observed.size > MAX_OBSERVED_TURNS_PER_STREAM) {
    const oldestTurnId = observed.keys().next().value;
    if (typeof oldestTurnId !== 'string') break;
    observed.delete(oldestTurnId);
  }
  observedTurnsByStream.set(streamId, observed);
  latestTurnByStream.set(streamId, turnId);
  return true;
}

/** Forget a deleted stream so a later conversation with the same ID starts at cursor zero. */
export function forgetChatEventStream(streamId: string): void {
  appliedCursorByStream.delete(streamId);
  terminalByStream.delete(streamId);
  latestTurnByStream.delete(streamId);
  observedTurnsByStream.delete(streamId);
  generationByStream.set(streamId, (generationByStream.get(streamId) ?? 0) + 1);
}

export function resetChatEventCursorsForTest(): void {
  appliedCursorByStream.clear();
  generationByStream.clear();
  terminalByStream.clear();
  latestTurnByStream.clear();
  observedTurnsByStream.clear();
}
