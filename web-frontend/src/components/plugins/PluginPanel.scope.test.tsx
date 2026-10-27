// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const fixture = vi.hoisted(() => ({
  current: { id: 'alpha', product_data_generation: 'generation-1' },
  list: vi.fn(),
  themes: vi.fn(),
  outputStyles: vi.fn(),
  reload: vi.fn(),
  activateTheme: vi.fn(),
  activateOutputStyle: vi.fn(),
  validate: vi.fn(),
  applyTheme: vi.fn(),
}));
vi.mock('../../api/endpoints', () => ({
  extensionRequestScope: (workspace: typeof fixture.current) => ({
    workspace_id: workspace.id,
    workspace_generation: workspace.product_data_generation,
    sender_id: null,
    sender_incarnation: null,
  }),
  pluginApi: fixture,
}));
vi.mock('../../stores/workspaceStore', () => ({
  useWorkspaceStore: Object.assign(
    (selector: (state: { current: typeof fixture.current }) => unknown) =>
      selector({ current: fixture.current }),
    { getState: () => ({ current: fixture.current }) }
  ),
}));
vi.mock('../../stores/uiStore', () => ({
  applyPluginTheme: fixture.applyTheme,
  useUiStore: (selector: (state: { activePluginTheme: null }) => unknown) =>
    selector({ activePluginTheme: null }),
}));
import { PluginPanel } from './PluginPanel';

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<T>((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
}
function listing(name: string) {
  return {
    plugins: [
      {
        name,
        version: '1',
        enabled: true,
        description: '',
        display_name: null,
        capabilities: [],
        keywords: [],
        dependencies: [],
        config: {},
        config_values: {},
      },
    ],
    omitted: 0,
  };
}
beforeEach(() => {
  fixture.current = { id: 'alpha', product_data_generation: 'generation-1' };
  fixture.list.mockReset().mockResolvedValue(listing('alpha-plugin'));
  fixture.themes
    .mockReset()
    .mockResolvedValue({ themes: [{ name: 'palette', plugin: 'test' }], active: null, omitted: 0 });
  fixture.outputStyles
    .mockReset()
    .mockResolvedValue({ styles: [{ name: 'concise', plugin: 'test' }], active: null, omitted: 0 });
  fixture.reload.mockReset();
  fixture.activateTheme.mockReset();
  fixture.activateOutputStyle.mockReset();
  fixture.validate.mockReset();
  fixture.applyTheme.mockReset();
});
afterEach(cleanup);

describe('Plugin panel scope settlement', () => {
  it('does not revive an old alpha catalog after alpha-beta-alpha navigation', async () => {
    const old = deferred<ReturnType<typeof listing>>();
    fixture.list.mockReturnValueOnce(old.promise);
    const view = render(<PluginPanel />);
    await waitFor(() => expect(fixture.list).toHaveBeenCalledTimes(1));
    fixture.current = { id: 'beta', product_data_generation: 'generation-2' };
    fixture.list.mockResolvedValue(listing('beta-plugin'));
    view.rerender(<PluginPanel />);
    await screen.findByText('beta-plugin');
    fixture.current = { id: 'alpha', product_data_generation: 'generation-1' };
    fixture.list.mockResolvedValue(listing('alpha-return-plugin'));
    view.rerender(<PluginPanel />);
    await screen.findByText('alpha-return-plugin');
    await act(async () => old.resolve(listing('alpha-first-visit')));
    expect(screen.queryByText('alpha-first-visit')).toBeNull();
    expect(screen.getByText('alpha-return-plugin')).toBeTruthy();
  });

  it('keeps a departed validation report out of the current workspace', async () => {
    const old = deferred<object>();
    fixture.validate.mockReturnValueOnce(old.promise);
    const view = render(<PluginPanel />);
    await screen.findByText('alpha-plugin');
    fireEvent.click(screen.getByTitle('Plugin developer tools'));
    fireEvent.change(screen.getByPlaceholderText('Plugin directory'), {
      target: { value: '/alpha/plugin' },
    });
    fireEvent.click(screen.getByRole('button', { name: 'Validate' }));
    fixture.current = { id: 'beta', product_data_generation: 'generation-2' };
    fixture.list.mockResolvedValue(listing('beta-plugin'));
    view.rerender(<PluginPanel />);
    await screen.findByText('beta-plugin');
    await act(async () =>
      old.resolve({ valid: false, errors: ['old plugin failed'], components: [] })
    );
    expect(screen.queryByText('old plugin failed')).toBeNull();
    expect((screen.getByRole('button', { name: 'Validate' }) as HTMLButtonElement).disabled).toBe(
      false
    );
  });

  it('keeps the new catalog when an old catalog arrives late', async () => {
    const old = deferred<ReturnType<typeof listing>>();
    fixture.list.mockReturnValueOnce(old.promise);
    const view = render(<PluginPanel />);
    await waitFor(() => expect(fixture.list).toHaveBeenCalledTimes(1));
    fixture.current = { id: 'beta', product_data_generation: 'generation-2' };
    fixture.list.mockResolvedValue(listing('beta-plugin'));
    view.rerender(<PluginPanel />);
    await screen.findByText('beta-plugin');
    await act(async () => old.resolve(listing('late-alpha-plugin')));
    expect(screen.queryByText('late-alpha-plugin')).toBeNull();
    expect(screen.getByText('beta-plugin')).toBeTruthy();
  });
  it('does not apply old theme data after a workspace generation is replaced', async () => {
    const old = deferred<object>();
    fixture.themes.mockReturnValueOnce(old.promise);
    const view = render(<PluginPanel />);
    await waitFor(() => expect(fixture.list).toHaveBeenCalledTimes(1));
    fixture.current = { id: 'alpha', product_data_generation: 'generation-2' };
    fixture.list.mockResolvedValue(listing('new-generation-plugin'));
    view.rerender(<PluginPanel />);
    await screen.findByText('new-generation-plugin');
    const calls = fixture.applyTheme.mock.calls.length;
    await act(async () =>
      old.resolve({ themes: [{ name: 'retired-palette' }], active: 'retired-palette', omitted: 0 })
    );
    expect(fixture.applyTheme).toHaveBeenCalledTimes(calls);
    expect(screen.getByText('new-generation-plugin')).toBeTruthy();
  });
  it('ignores old catalog errors after switching workspace', async () => {
    const old = deferred<ReturnType<typeof listing>>();
    fixture.list.mockReturnValueOnce(old.promise);
    const view = render(<PluginPanel />);
    await waitFor(() => expect(fixture.list).toHaveBeenCalledTimes(1));
    fixture.current = { id: 'beta', product_data_generation: 'generation-2' };
    fixture.list.mockResolvedValue(listing('beta-plugin'));
    view.rerender(<PluginPanel />);
    await screen.findByText('beta-plugin');
    await act(async () => old.reject(new Error('departed catalog failed')));
    expect(screen.queryByText('departed catalog failed')).toBeNull();
    expect(screen.getByText('beta-plugin')).toBeTruthy();
  });
  it('does not refresh a departed target when its reload settles', async () => {
    const old = deferred<object>();
    fixture.reload.mockReturnValueOnce(old.promise);
    const view = render(<PluginPanel />);
    await screen.findByText('alpha-plugin');
    fireEvent.click(screen.getByTitle('Reload plugins'));
    fixture.current = { id: 'beta', product_data_generation: 'generation-2' };
    fixture.list.mockResolvedValue(listing('beta-plugin'));
    view.rerender(<PluginPanel />);
    await screen.findByText('beta-plugin');
    const calls = fixture.list.mock.calls.length;
    await act(async () => old.resolve({ success: true, message: 'alpha reload settled' }));
    expect(fixture.list).toHaveBeenCalledTimes(calls);
    expect(screen.queryByText('alpha reload settled')).toBeNull();
  });
  it('does not apply a delayed theme activation to a new workspace', async () => {
    const old = deferred<object>();
    fixture.activateTheme.mockReturnValueOnce(old.promise);
    const view = render(<PluginPanel />);
    await screen.findByText('alpha-plugin');
    fireEvent.change(screen.getAllByRole('combobox')[0]!, { target: { value: 'palette' } });
    fixture.current = { id: 'beta', product_data_generation: 'generation-2' };
    fixture.list.mockResolvedValue(listing('beta-plugin'));
    view.rerender(<PluginPanel />);
    await screen.findByText('beta-plugin');
    const calls = fixture.applyTheme.mock.calls.length;
    await act(async () => old.resolve({ active: 'palette', theme: { name: 'palette' } }));
    expect(fixture.applyTheme).toHaveBeenCalledTimes(calls);
    expect(screen.queryByText("Theme 'palette' activated")).toBeNull();
  });
  it('does not display an old output style activation in the current workspace', async () => {
    const old = deferred<object>();
    fixture.activateOutputStyle.mockReturnValueOnce(old.promise);
    const view = render(<PluginPanel />);
    await screen.findByText('alpha-plugin');
    fireEvent.change(screen.getByRole('combobox', { name: /Output style/ }), {
      target: { value: 'concise' },
    });
    fixture.current = { id: 'beta', product_data_generation: 'generation-2' };
    fixture.list.mockResolvedValue(listing('beta-plugin'));
    view.rerender(<PluginPanel />);
    await screen.findByText('beta-plugin');
    await act(async () => old.resolve({ active: 'concise' }));
    expect(screen.queryByText("Output style 'concise' activated")).toBeNull();
  });
  it('does not request another catalog after unmounting a pending reload', async () => {
    const old = deferred<object>();
    fixture.reload.mockReturnValueOnce(old.promise);
    const view = render(<PluginPanel />);
    await screen.findByText('alpha-plugin');
    fireEvent.click(screen.getByTitle('Reload plugins'));
    view.unmount();
    const calls = fixture.list.mock.calls.length;
    await act(async () => old.resolve({ success: true }));
    expect(fixture.list).toHaveBeenCalledTimes(calls);
  });
  it('keeps current workspace activation and reload behavior', async () => {
    fixture.activateTheme.mockResolvedValue({ active: 'palette', theme: { name: 'palette' } });
    fixture.reload.mockResolvedValue({ success: true, message: 'current reload settled' });
    render(<PluginPanel />);
    await screen.findByText('alpha-plugin');
    fireEvent.change(screen.getAllByRole('combobox')[0]!, { target: { value: 'palette' } });
    await screen.findByText("Theme 'palette' activated");
    expect(fixture.applyTheme).toHaveBeenLastCalledWith({ name: 'palette' });
    fireEvent.click(screen.getByTitle('Reload plugins'));
    await screen.findByText('current reload settled');
    expect(fixture.reload.mock.calls[0]?.[0]).toMatchObject({
      workspace_id: 'alpha',
      workspace_generation: 'generation-1',
    });
  });
});
