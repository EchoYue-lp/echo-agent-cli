// @vitest-environment jsdom
import { beforeEach, describe, expect, it } from 'vitest';

Object.defineProperty(window, 'matchMedia', {
  configurable: true,
  value: () => ({ matches: false }),
});

const { useUiStore } = await import('./uiStore');

describe('settings navigation', () => {
  beforeEach(() => {
    useUiStore.setState({ settingsOpen: false, activeSettingsTab: 'providers' });
  });

  it('opens the general settings entry on the overview tab', () => {
    useUiStore.getState().openSettings();

    expect(useUiStore.getState().settingsOpen).toBe(true);
    expect(useUiStore.getState().activeSettingsTab).toBe('overview');
  });

  it('keeps direct model settings navigation explicit', () => {
    useUiStore.getState().setActiveSettingsTab('providers');

    expect(useUiStore.getState().settingsOpen).toBe(true);
    expect(useUiStore.getState().activeSettingsTab).toBe('providers');
  });
});
