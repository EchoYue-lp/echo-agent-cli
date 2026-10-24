import { beforeEach, describe, expect, it, vi } from 'vitest';
const bridge = vi.hoisted(() => ({ apiInvoke: vi.fn(), isTauri: vi.fn(() => true) }));
const http = vi.hoisted(() => ({ get: vi.fn(), post: vi.fn(), del: vi.fn() }));
vi.mock('../lib/tauri-bridge', () => bridge);
vi.mock('./client', () => http);
import { terminalApi } from './endpoints';

beforeEach(() => {
  vi.clearAllMocks();
  bridge.isTauri.mockReturnValue(true);
});
describe('interactive terminal endpoint contracts', () => {
  it('sends independent target fields and preserves native session metadata and close proof', async () => {
    const target = { kind: 'ssh' as const, host: 'existing-alias', user: 'someone', port: 2222 };
    const session = { id: 'original-id', pid: 42, target };
    bridge.apiInvoke
      .mockResolvedValueOnce(session)
      .mockResolvedValueOnce({ success: true, closed: false });
    await expect(
      terminalApi.create({ id: session.id, target, rows: 30, cols: 100 })
    ).resolves.toEqual(session);
    expect(bridge.apiInvoke).toHaveBeenCalledWith('create_terminal', {
      id: session.id,
      target,
      rows: 30,
      cols: 100,
    });
    await expect(terminalApi.close(session.id)).resolves.toEqual({ success: true, closed: false });
    expect(http.post).not.toHaveBeenCalled();
  });
  it('retains the existing HTTP local destination and normalizes missing local metadata', async () => {
    bridge.isTauri.mockReturnValue(false);
    http.post.mockResolvedValue({ id: 'http-assigned-id', pid: 12 });
    await expect(terminalApi.create({ id: 'client-ui-id', cwd: '/work' })).resolves.toEqual({
      id: 'http-assigned-id',
      pid: 12,
      target: { kind: 'local' },
    });
    expect(http.post).toHaveBeenCalledExactlyOnceWith('/terminal', { cwd: '/work' });
    await terminalApi.close('http-assigned-id');
    expect(http.del).toHaveBeenCalledWith('/terminal/http-assigned-id');
  });
  it('never routes SSH to the legacy HTTP local terminal', async () => {
    bridge.isTauri.mockReturnValue(false);
    await expect(terminalApi.create({ target: { kind: 'ssh', host: 'alias' } })).rejects.toThrow(
      'desktop'
    );
    expect(bridge.apiInvoke).not.toHaveBeenCalled();
    expect(http.post).not.toHaveBeenCalled();
  });
});
