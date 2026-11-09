// @vitest-environment jsdom
import { describe, expect, it, vi } from 'vitest';

vi.hoisted(() => {
  localStorage.setItem('setup-before-hoisted-import', 'ready');
});

const domWindow = (globalThis as typeof globalThis & { jsdom: { window: Window } }).jsdom.window;

describe('browser test Storage ownership', () => {
  it('provides the real per-file jsdom stores before hoisted imports', () => {
    expect(localStorage).toBe(domWindow.localStorage);
    expect(sessionStorage).toBe(domWindow.sessionStorage);
    expect(localStorage.getItem('setup-before-hoisted-import')).toBe('ready');
    sessionStorage.setItem('session-identity', 'same-origin');
    expect(domWindow.sessionStorage.getItem('session-identity')).toBe('same-origin');
  });

  it.each(['undefined', 'throwing'] as const)(
    'rebinds existing %s host properties without replacing the underlying DOM data',
    async (mode) => {
      const originals = new Map(
        (['localStorage', 'sessionStorage'] as const).map((key) => [
          key,
          Object.getOwnPropertyDescriptor(globalThis, key),
        ])
      );
      domWindow.localStorage.setItem('retained-dom-data', 'preserved');
      try {
        for (const key of ['localStorage', 'sessionStorage'] as const) {
          Object.defineProperty(globalThis, key, {
            configurable: true,
            get: () => {
              if (mode === 'throwing') throw new Error('Unavailable host Storage');
              return undefined;
            },
          });
        }
        vi.resetModules();
        await import('./setup');
        expect(localStorage).toBe(domWindow.localStorage);
        expect(sessionStorage).toBe(domWindow.sessionStorage);
        expect(localStorage.getItem('retained-dom-data')).toBe('preserved');
      } finally {
        for (const [key, descriptor] of originals) {
          if (descriptor) Object.defineProperty(globalThis, key, descriptor);
        }
      }
    }
  );
});
