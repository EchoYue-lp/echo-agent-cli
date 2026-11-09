// @vitest-environment node
import { expect, it, vi } from 'vitest';

it('leaves native host Storage descriptors untouched without a jsdom environment', async () => {
  expect('jsdom' in globalThis).toBe(false);
  const local = Object.getOwnPropertyDescriptor(globalThis, 'localStorage');
  const session = Object.getOwnPropertyDescriptor(globalThis, 'sessionStorage');
  vi.resetModules();
  await import('./setup');
  expect(Object.getOwnPropertyDescriptor(globalThis, 'localStorage')).toEqual(local);
  expect(Object.getOwnPropertyDescriptor(globalThis, 'sessionStorage')).toEqual(session);
});
