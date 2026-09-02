// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const mocks = vi.hoisted(() => ({ invoke: vi.fn() }));

vi.mock('@tauri-apps/api/core', () => ({ invoke: mocks.invoke }));

describe('apiInvoke', () => {
  beforeEach(() => {
    vi.resetModules();
    mocks.invoke.mockReset();
    window.__TAURI_INTERNALS__ = {};
  });

  afterEach(() => {
    delete window.__TAURI_INTERNALS__;
  });

  it('settles with AbortError when the caller timeout fires', async () => {
    mocks.invoke.mockReturnValue(new Promise(() => undefined));
    const { apiInvoke } = await import('./tauri-bridge');
    const controller = new AbortController();
    const pending = apiInvoke('slow_command', undefined, controller.signal);

    controller.abort();

    await expect(pending).rejects.toMatchObject({ name: 'AbortError' });
  });

  it('returns the native result before the abort boundary', async () => {
    mocks.invoke.mockResolvedValue({ success: true });
    const { apiInvoke } = await import('./tauri-bridge');
    const controller = new AbortController();

    await expect(apiInvoke('fast_command', {}, controller.signal)).resolves.toEqual({
      success: true,
    });
  });
});
