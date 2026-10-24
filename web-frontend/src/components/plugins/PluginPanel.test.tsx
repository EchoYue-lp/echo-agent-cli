// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
const api = vi.hoisted(() => ({
  list: vi.fn(async () => ({ plugins: [], omitted: 0 })),
  themes: vi.fn(async () => ({ themes: [], omitted: 0, active: null })),
  outputStyles: vi.fn(async () => ({ styles: [], omitted: 0, active: null })),
  validate: vi.fn(async () => ({
    valid: true,
    name: 'mixed',
    components: ['skills'],
    errors: ['LSP component has been retired from EKO'],
  })),
}));
vi.mock('../../api/endpoints', () => ({
  pluginApi: api,
  extensionRequestScope: () => ({ workspace_id: 'global', workspace_generation: 'global' }),
}));
vi.mock('../../stores/workspaceStore', () => ({
  useWorkspaceStore: (selector: (s: { current: null }) => unknown) => selector({ current: null }),
}));
vi.mock('../../stores/uiStore', () => ({
  applyPluginTheme: vi.fn(),
  useUiStore: (selector: (s: { activePluginTheme: null }) => unknown) =>
    selector({ activePluginTheme: null }),
}));
import { PluginPanel } from './PluginPanel';
afterEach(cleanup);
describe('retired plugin component diagnostics', () => {
  it('shows retirement diagnostics even when retained plugin components validate', async () => {
    render(<PluginPanel />);
    fireEvent.click(await screen.findByTitle('Plugin developer tools'));
    fireEvent.change(screen.getByPlaceholderText('Plugin directory'), {
      target: { value: '/isolated/mixed' },
    });
    fireEvent.click(screen.getByRole('button', { name: 'Validate' }));
    expect(await screen.findByText(/LSP component has been retired from EKO/)).toBeTruthy();
    expect(api.validate).toHaveBeenCalled();
  });
});
