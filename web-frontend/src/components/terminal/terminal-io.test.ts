import { describe, expect, it, vi } from 'vitest';
import { boundedTerminalQueue, decodeTerminalBytes, encodeTerminalBytes } from './terminal-io';

function deferred() {
  let release = () => {};
  const promise = new Promise<void>((resolve) => {
    release = resolve;
  });
  return { promise, release };
}
const tick = async () => {
  for (let i = 0; i < 8; i++) await Promise.resolve();
};

describe('bounded terminal byte stream', () => {
  it('preserves split Chinese and emoji bytes until the streaming consumer decodes them', () => {
    const original = new TextEncoder().encode('中🙂文');
    const decoder = new TextDecoder();
    let rendered = '';
    for (const byte of original) {
      const transmitted = decodeTerminalBytes(encodeTerminalBytes(Uint8Array.of(byte)));
      rendered += decoder.decode(transmitted, { stream: true });
    }
    rendered += decoder.decode();
    expect(rendered).toBe('中🙂文');
  });
  it('serializes writes and includes the outstanding write in its budget', async () => {
    const first = deferred();
    const write = vi
      .fn()
      .mockImplementationOnce(() => first.promise)
      .mockResolvedValue(undefined);
    const fail = vi.fn();
    const queue = boundedTerminalQueue(write, fail, 4, 'visible gap');
    expect(queue.push(Uint8Array.of(1, 2))).toBe(true);
    expect(queue.push(Uint8Array.of(3, 4))).toBe(true);
    await tick();
    expect(write).toHaveBeenCalledTimes(1);
    expect(queue.pendingBytes()).toBe(4);
    expect(queue.push(Uint8Array.of(5))).toBe(false);
    expect(fail).toHaveBeenCalledExactlyOnceWith('visible gap');
    first.release();
    await tick();
    expect(write).toHaveBeenCalledTimes(1);
    expect(queue.pendingBytes()).toBe(0);
    expect(queue.push(Uint8Array.of(6))).toBe(false);
  });
  it('never automatically resends after an uncertain write failure', async () => {
    const write = vi.fn().mockRejectedValue(new Error('secret input must not appear'));
    const fail = vi.fn();
    const queue = boundedTerminalQueue(write, fail, 8, 'overflow');
    queue.push(Uint8Array.of(1));
    queue.push(Uint8Array.of(2));
    await tick();
    expect(write).toHaveBeenCalledTimes(1);
    expect(fail).toHaveBeenCalledTimes(1);
    expect(String(fail.mock.calls)).not.toContain('secret input');
    expect(queue.pendingBytes()).toBe(0);
  });
  it('bounds tiny chunk backlog independently of its byte budget', () => {
    const fail = vi.fn();
    const queue = boundedTerminalQueue(() => new Promise(() => {}), fail, 10000, 'too many chunks');
    for (let i = 0; i < 258; i++) queue.push(Uint8Array.of(i % 256));
    expect(fail).toHaveBeenCalledExactlyOnceWith('too many chunks');
    queue.dispose();
  });
  it('does not dispatch a queued microtask after disposal', async () => {
    const write = vi.fn();
    const queue = boundedTerminalQueue(write, vi.fn(), 8, 'overflow');
    queue.push(Uint8Array.of(1));
    queue.dispose();
    await tick();
    expect(write).not.toHaveBeenCalled();
  });
});
