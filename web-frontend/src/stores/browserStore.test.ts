import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { BrowserCommand, ExtensionCommandReceipt } from '../generated';
import { viewAddress, viewAddressKey } from '../lib/viewAddress';

const bridge = vi.hoisted(() => ({
  apiInvoke: vi.fn(),
  errorMessage: vi.fn((error: unknown) => (error instanceof Error ? error.message : String(error))),
  isTauri: vi.fn(() => true),
}));

vi.mock('../lib/tauri-bridge', () => bridge);

import { useBrowserStore } from './browserStore';

const addressKey = viewAddressKey(viewAddress('workspace-a', 'conversation-a'));
const requestScope = {
  workspace_id: 'workspace-a',
  workspace_generation: 'generation-a',
  sender_id: 'sender-a',
  sender_incarnation: 'incarnation-a',
};
const meta = {
  request_id: 'request-1',
  operation_id: 'operation-1',
  authority_scope: 'workspace-a',
  workspace_generation: 'generation-a',
  sender_id: null,
  sender_incarnation: null,
  status: 'settled' as const,
  error: null,
};

function mockReceipt(
  status: ExtensionCommandReceipt['meta']['status'],
  error: string | null,
  message: string
) {
  bridge.apiInvoke.mockImplementation(
    (
      _command: string,
      args: {
        request: {
          request_id: string;
          operation_id: string;
          scope: typeof requestScope;
        };
      }
    ) => {
      const request = args.request;
      return Promise.resolve({
        extension: 'browser',
        meta: {
          ...meta,
          request_id: request.request_id,
          operation_id: request.operation_id,
          authority_scope: request.scope.workspace_id,
          workspace_generation: request.scope.workspace_generation,
          sender_id: request.scope.sender_id,
          sender_incarnation: request.scope.sender_incarnation,
          status,
          error,
        },
        receipt:
          status === 'failed' || status === 'degraded' ? null : { action: 'browser', message },
      } satisfies ExtensionCommandReceipt);
    }
  );
}

describe('Browser Extension receipts', () => {
  beforeEach(() => {
    bridge.apiInvoke.mockReset();
    bridge.isTauri.mockReturnValue(true);
    useBrowserStore.setState({
      views: {},
      commandErrors: {},
      commandPending: {},
      commandReceipts: {},
      chromeConnected: false,
    });
  });

  it('retains committed commands as pending instead of reporting success', async () => {
    mockReceipt('committed', null, 'Navigation settlement pending');

    await useBrowserStore
      .getState()
      .navigate(requestScope, 'conversation-a', 'https://example.com');

    expect(useBrowserStore.getState().commandPending[addressKey]).toBe(
      'Navigation settlement pending'
    );
    expect(useBrowserStore.getState().commandErrors[addressKey]).toBe('');
    expect(useBrowserStore.getState().commandReceipts[addressKey]).toBe('');
  });

  it('rejects degraded receipts and exposes the typed error', async () => {
    mockReceipt('degraded', 'Browser fanout failed', '');

    await useBrowserStore.getState().reload(requestScope, 'conversation-a');

    expect(useBrowserStore.getState().commandErrors[addressKey]).toBe('Browser fanout failed');
    expect(useBrowserStore.getState().commandPending[addressKey]).toBe('');
    expect(useBrowserStore.getState().commandReceipts[addressKey]).toBe('');
  });

  it('routes the complete Browser union through the typed dispatcher with exact scope', async () => {
    const commands: BrowserCommand[] = [
      { action: 'status' },
      { action: 'managed' },
      { action: 'chrome' },
      { action: 'navigate', url: 'https://example.com' },
      { action: 'snapshot', filename: 'artifacts/page.md' },
      {
        action: 'click_target',
        target: 'button[ref=e1]',
        element: 'Save',
        button: 'left',
        double_click: false,
        effect: 'publish',
      },
      {
        action: 'fill',
        target: 'input[ref=e2]',
        text: 'typed value',
        element: 'Title',
        submit: true,
        slowly: true,
        effect: 'sensitive_submit',
      },
      { action: 'back' },
      { action: 'reload' },
      { action: 'screenshot' },
      { action: 'click', x: 12.5, y: 24 },
      {
        action: 'type_at',
        x: 42,
        y: 84,
        text: 'coordinates',
        submit: false,
        slowly: false,
        effect: 'none',
      },
      { action: 'scroll', delta_x: 0, delta_y: -320 },
      { action: 'tabs', tab_action: 'select', index: 2, url: null },
      { action: 'console', level: 'warning', contains: 'Extension' },
      { action: 'network', method: 'POST', status: 201, contains: '/api/' },
      { action: 'dom_inspect', target: 'main', text: null, max_depth: 4 },
      {
        action: 'performance_trace',
        trace_action: 'stop',
        path: 'artifacts/trace.zip',
      },
      { action: 'developer_mode', enabled: true },
      { action: 'stop' },
    ];
    mockReceipt('settled', null, 'Browser command completed.');

    for (const command of commands) {
      const result = await useBrowserStore
        .getState()
        .execute(requestScope, 'conversation-a', command);
      expect(result.status).toBe('settled');
    }

    expect(bridge.apiInvoke).toHaveBeenCalledTimes(commands.length);
    commands.forEach((command, index) => {
      const call = bridge.apiInvoke.mock.calls[index];
      expect(call?.[0]).toBe('execute_extension_command');
      expect(call?.[1]).toMatchObject({
        workspaceId: 'workspace-a',
        workspaceGeneration: 'generation-a',
        conversationId: 'conversation-a',
        request: {
          request_id: expect.any(String),
          operation_id: expect.any(String),
          scope: requestScope,
          extension: 'browser',
          command,
        },
      });
    });
    expect(useBrowserStore.getState().commandReceipts[addressKey]).toBe(
      'Browser command completed.'
    );
  });

  it('maps existing toolbar actions into generated Browser commands', async () => {
    mockReceipt('settled', null, 'Browser command completed.');

    const store = useBrowserStore.getState();
    await store.navigate(requestScope, 'conversation-a', 'https://example.com');
    await store.clickAt(requestScope, 'conversation-a', 10, 20);
    await store.scroll(requestScope, 'conversation-a', 5, -40);
    await store.selectTab(requestScope, 'conversation-a', 3);

    expect(bridge.apiInvoke.mock.calls.map((call) => call[1]?.request.command)).toEqual([
      { action: 'navigate', url: 'https://example.com' },
      { action: 'click', x: 10, y: 20 },
      { action: 'scroll', delta_x: 5, delta_y: -40 },
      { action: 'tabs', tab_action: 'select', index: 3, url: null },
    ]);
  });
});
