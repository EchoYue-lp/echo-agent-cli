// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const fixture = vi.hoisted(() => ({
  current: { id: 'alpha', product_data_generation: 'generation-1' },
  list: vi.fn(),
  events: vi.fn(),
  reload: vi.fn(),
  test: vi.fn(),
}));

vi.mock('../../api/endpoints', () => ({
  extensionRequestScope: (workspace: typeof fixture.current) => ({
    workspace_id: workspace.id,
    workspace_generation: workspace.product_data_generation,
    sender_id: null,
    sender_incarnation: null,
  }),
  hooksApi: fixture,
}));
vi.mock('../../stores/workspaceStore', () => ({
  useWorkspaceStore: Object.assign(
    (selector: (state: { current: typeof fixture.current }) => unknown) =>
      selector({ current: fixture.current }),
    { getState: () => ({ current: fixture.current }) }
  ),
}));

import { HooksPanel } from './HooksPanel';

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<T>((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
}

function listing(source: string) {
  return { sources: [{ source, rule_count: 1 }], omitted: 0 };
}

beforeEach(() => {
  fixture.current = { id: 'alpha', product_data_generation: 'generation-1' };
  fixture.list.mockReset().mockResolvedValue(listing('plugin:alpha'));
  fixture.events.mockReset().mockResolvedValue(['PreToolUse']);
  fixture.reload.mockReset();
  fixture.test.mockReset();
});
afterEach(cleanup);

describe('Hook panel scope settlement', () => {
  it('does not start another list when a pending reload settles after unmount', async () => {
    const pending = deferred<object>();
    fixture.reload.mockReturnValueOnce(pending.promise);
    const view = render(<HooksPanel />);
    await screen.findByText('plugin:alpha');
    fireEvent.click(screen.getByRole('button', { name: 'Reload hooks from config' }));
    expect(fixture.reload).toHaveBeenCalledTimes(1);
    view.unmount();
    const callsBeforeSettlement = fixture.list.mock.calls.length;
    await act(async () =>
      pending.resolve({
        success: true,
        loaded_from: [],
        loaded_from_omitted: 0,
        rule_count: 1,
        message: 'unmounted reload settled',
      })
    );
    expect(fixture.list).toHaveBeenCalledTimes(callsBeforeSettlement);
  });

  it('keeps the current list when an older workspace list settles later', async () => {
    const old = deferred<ReturnType<typeof listing>>();
    fixture.list.mockReturnValueOnce(old.promise);
    const view = render(<HooksPanel />);
    await waitFor(() => expect(fixture.list).toHaveBeenCalledTimes(1));
    fixture.current = { id: 'beta', product_data_generation: 'generation-2' };
    fixture.list.mockResolvedValue(listing('plugin:beta'));
    view.rerender(<HooksPanel />);
    await screen.findByText('plugin:beta');
    await act(async () => old.resolve(listing('plugin:late-alpha')));
    expect(screen.queryByText('plugin:late-alpha')).toBeNull();
    expect(screen.getByText('plugin:beta')).toBeTruthy();
  });

  it('does not display a rejected old list over the current workspace', async () => {
    const old = deferred<ReturnType<typeof listing>>();
    fixture.list.mockReturnValueOnce(old.promise);
    const view = render(<HooksPanel />);
    await waitFor(() => expect(fixture.list).toHaveBeenCalledTimes(1));
    fixture.current = { id: 'beta', product_data_generation: 'generation-2' };
    fixture.list.mockResolvedValue(listing('plugin:beta'));
    view.rerender(<HooksPanel />);
    await screen.findByText('plugin:beta');
    await act(async () => old.reject(new Error('old target is gone')));
    expect(screen.queryByText('old target is gone')).toBeNull();
    expect(screen.getByText('plugin:beta')).toBeTruthy();
  });

  it('fences a replaced generation of the same workspace', async () => {
    const old = deferred<ReturnType<typeof listing>>();
    fixture.list.mockReturnValueOnce(old.promise);
    const view = render(<HooksPanel />);
    await waitFor(() => expect(fixture.list).toHaveBeenCalledTimes(1));
    fixture.current = { id: 'alpha', product_data_generation: 'generation-2' };
    fixture.list.mockResolvedValue(listing('plugin:replacement'));
    view.rerender(<HooksPanel />);
    await screen.findByText('plugin:replacement');
    await act(async () => old.resolve(listing('plugin:retired-generation')));
    expect(screen.queryByText('plugin:retired-generation')).toBeNull();
    expect(screen.getByText('plugin:replacement')).toBeTruthy();
  });

  it('does not refresh the departed workspace after its reload settles', async () => {
    const old = deferred<object>();
    fixture.reload.mockReturnValueOnce(old.promise);
    const view = render(<HooksPanel />);
    await screen.findByText('plugin:alpha');
    fireEvent.click(screen.getByRole('button', { name: 'Reload hooks from config' }));
    fixture.current = { id: 'beta', product_data_generation: 'generation-2' };
    fixture.list.mockResolvedValue(listing('plugin:beta'));
    view.rerender(<HooksPanel />);
    await screen.findByText('plugin:beta');
    const callsBeforeSettlement = fixture.list.mock.calls.length;
    await act(async () =>
      old.resolve({
        success: true,
        loaded_from: [],
        loaded_from_omitted: 0,
        rule_count: 1,
        message: 'alpha reloaded',
      })
    );
    expect(fixture.list).toHaveBeenCalledTimes(callsBeforeSettlement);
    expect(screen.queryByText('alpha reloaded')).toBeNull();
    expect(screen.getByText('plugin:beta')).toBeTruthy();
  });

  it('ignores an old dry-run result and leaves the new target usable', async () => {
    const old = deferred<object>();
    fixture.test.mockReturnValueOnce(old.promise);
    const view = render(<HooksPanel />);
    await screen.findByText('plugin:alpha');
    fireEvent.click(screen.getByRole('button', { name: 'Dry-run hook matching' }));
    fixture.current = { id: 'beta', product_data_generation: 'generation-2' };
    fixture.list.mockResolvedValue(listing('plugin:beta'));
    view.rerender(<HooksPanel />);
    await screen.findByText('plugin:beta');
    await act(async () =>
      old.resolve({
        event: 'PreToolUse',
        matches: [{ source: 'plugin:old-run', matcher: '*', action: 'allow' }],
        matches_omitted: 0,
      })
    );
    expect(screen.queryByText(/plugin:old-run/)).toBeNull();
    expect(
      (
        screen.getByRole('button', {
          name: 'Dry-run hook matching',
        }) as HTMLButtonElement
      ).disabled
    ).toBe(false);
  });

  it('does not revive a first alpha visit after alpha-beta-alpha navigation', async () => {
    const old = deferred<ReturnType<typeof listing>>();
    fixture.list.mockReturnValueOnce(old.promise);
    const view = render(<HooksPanel />);
    await waitFor(() => expect(fixture.list).toHaveBeenCalledTimes(1));
    fixture.current = { id: 'beta', product_data_generation: 'generation-2' };
    fixture.list.mockResolvedValue(listing('plugin:beta'));
    view.rerender(<HooksPanel />);
    await screen.findByText('plugin:beta');
    fixture.current = { id: 'alpha', product_data_generation: 'generation-1' };
    fixture.list.mockResolvedValue(listing('plugin:alpha-return'));
    view.rerender(<HooksPanel />);
    await screen.findByText('plugin:alpha-return');
    await act(async () => old.resolve(listing('plugin:first-visit')));
    expect(screen.queryByText('plugin:first-visit')).toBeNull();
    expect(screen.getByText('plugin:alpha-return')).toBeTruthy();
  });

  it('keeps successful current-scope reload and matching behavior', async () => {
    fixture.reload.mockResolvedValue({
      success: true,
      loaded_from: [],
      loaded_from_omitted: 0,
      rule_count: 1,
      message: 'current hooks reloaded',
    });
    fixture.test.mockResolvedValue({
      event: 'PreToolUse',
      matches: [{ source: 'plugin:current', matcher: '*', action: 'ask' }],
      matches_omitted: 0,
    });
    render(<HooksPanel />);
    await screen.findByText('plugin:alpha');
    fireEvent.click(screen.getByRole('button', { name: 'Reload hooks from config' }));
    await screen.findByText('current hooks reloaded');
    fireEvent.click(screen.getByRole('button', { name: 'Dry-run hook matching' }));
    expect(await screen.findByText(/plugin:current: ask/)).toBeTruthy();
    expect(fixture.reload.mock.calls[0]?.[0]).toMatchObject({
      workspace_id: 'alpha',
      workspace_generation: 'generation-1',
    });
  });
});
