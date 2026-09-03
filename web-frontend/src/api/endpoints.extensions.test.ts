import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { ExtensionCommandReceipt, ExtensionCommandRequest } from '../generated';

const bridge = vi.hoisted(() => ({
  apiInvoke: vi.fn(),
  isTauri: vi.fn(() => true),
}));

vi.mock('../lib/tauri-bridge', () => bridge);

import {
  assertExtensionReceiptScope,
  browserExtensionDisposition,
  extensionApi,
  hooksApi,
  lspApi,
  mcpConfigDisposition,
  pluginApi,
  skillsApi,
} from './endpoints';

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

const requestScope = {
  workspace_id: 'workspace-a',
  workspace_generation: 'generation-a',
  sender_id: null,
  sender_incarnation: null,
};

describe('typed Extension IPC adapters', () => {
  beforeEach(() => {
    bridge.apiInvoke.mockReset();
    bridge.isTauri.mockReturnValue(true);
  });

  it('preserves exact workspace and typed request for generic execution', async () => {
    const request: ExtensionCommandRequest = {
      request_id: 'request-1',
      operation_id: 'operation-1',
      scope: requestScope,
      extension: 'browser',
      command: { action: 'tabs', tab_action: 'select', index: 3, url: null },
    };
    const receipt: ExtensionCommandReceipt = {
      extension: 'browser',
      meta,
      receipt: { action: 'tabs', message: 'Browser tabs completed.' },
    };
    bridge.apiInvoke.mockResolvedValue(receipt);

    await expect(extensionApi.execute(requestScope, 'conversation-a', request)).resolves.toEqual(
      receipt
    );
    expect(bridge.apiInvoke).toHaveBeenCalledWith('execute_extension_command', {
      workspaceId: 'workspace-a',
      workspaceGeneration: 'generation-a',
      conversationId: 'conversation-a',
      request,
    });
  });

  it('fails closed when a receipt does not match the exact workspace incarnation', () => {
    const receipt: ExtensionCommandReceipt = {
      extension: 'skills',
      meta: { ...meta, workspace_generation: 'generation-b' },
      receipt: { action: 'listed', skills: { items: [], omitted: 0 } },
    };
    expect(() => assertExtensionReceiptScope(receipt, requestScope)).toThrow(
      'exact request scope and identity'
    );
  });

  it('routes Skill search through the typed endpoint and preserves the bounded receipt', async () => {
    bridge.apiInvoke.mockImplementation(async (_command: string, args: any) => ({
      extension: 'skills',
      meta: {
        ...meta,
        request_id: args.request.request_id,
        operation_id: args.request.operation_id,
      },
      receipt: {
        action: 'searched',
        query: 'rust',
        skills: { items: [], omitted: 7 },
      },
    }));

    const receipt = await skillsApi.search(requestScope, 'rust');

    expect(receipt).toMatchObject({
      extension: 'skills',
      receipt: { action: 'searched', query: 'rust', skills: { omitted: 7 } },
    });
    expect(bridge.apiInvoke).toHaveBeenCalledWith(
      'execute_extension_command',
      expect.objectContaining({
        workspaceId: 'workspace-a',
        workspaceGeneration: 'generation-a',
        conversationId: 'gui-settings-skills',
        request: expect.objectContaining({
          scope: requestScope,
          extension: 'skills',
          command: { action: 'search', query: 'rust' },
        }),
      })
    );
  });

  it('routes every GUI Skill operation through one exact-scope typed endpoint', async () => {
    bridge.apiInvoke.mockImplementation(async (_command: string, args: any) => ({
      extension: 'skills',
      meta: {
        ...meta,
        request_id: args.request.request_id,
        operation_id: args.request.operation_id,
      },
      receipt: { action: 'listed', skills: { items: [], omitted: 0 } },
    }));

    await Promise.all([
      skillsApi.list(requestScope),
      skillsApi.search(requestScope, 'rust'),
      skillsApi.get(requestScope, 'rust'),
      skillsApi.load(requestScope, '/tmp/rust'),
      skillsApi.uninstall(requestScope, 'rust'),
      skillsApi.enable(requestScope, 'rust'),
      skillsApi.disable(requestScope, 'rust'),
      skillsApi.refresh(requestScope),
      skillsApi.checkUpdates(requestScope, 'rust'),
      skillsApi.sync(requestScope, 'rust', true),
    ]);

    expect(bridge.apiInvoke.mock.calls.map(([, args]) => args.request.command.action)).toEqual([
      'list',
      'search',
      'info',
      'install',
      'uninstall',
      'enable',
      'disable',
      'refresh',
      'check_updates',
      'sync',
    ]);
    for (const [command, args] of bridge.apiInvoke.mock.calls) {
      expect(command).toBe('execute_extension_command');
      expect(args).toMatchObject({
        workspaceId: requestScope.workspace_id,
        workspaceGeneration: requestScope.workspace_generation,
        request: { scope: requestScope, extension: 'skills' },
      });
    }
  });

  it('routes Skill activation to the exact current conversation', async () => {
    bridge.apiInvoke.mockImplementation(async (_command: string, args: any) => ({
      extension: 'skills',
      meta: {
        ...meta,
        request_id: args.request.request_id,
        operation_id: args.request.operation_id,
      },
      receipt: { action: 'activated', name: 'git-workflow' },
    }));

    await skillsApi.activate(requestScope, 'conversation-current', 'git-workflow');

    expect(bridge.apiInvoke).toHaveBeenCalledWith(
      'execute_extension_command',
      expect.objectContaining({
        conversationId: 'conversation-current',
        request: expect.objectContaining({
          command: { action: 'activate', name: 'git-workflow' },
        }),
      })
    );
  });

  it('rejects failed and degraded Browser receipts instead of dropping status', () => {
    for (const status of ['failed', 'degraded'] as const) {
      const receipt: ExtensionCommandReceipt = {
        extension: 'browser',
        meta: { ...meta, status, error: `browser ${status}` },
        receipt: null,
      };
      expect(() => browserExtensionDisposition(receipt)).toThrow(`browser ${status}`);
    }
  });

  it('projects committed Browser receipts as pending, never settled', () => {
    const receipt: ExtensionCommandReceipt = {
      extension: 'browser',
      meta: { ...meta, status: 'committed' },
      receipt: { action: 'navigate', message: 'Navigation policy committed' },
    };

    expect(browserExtensionDisposition(receipt)).toEqual({
      status: 'pending',
      message: 'Navigation policy committed',
    });
  });

  it('projects committed MCP config as pending, never success', () => {
    expect(
      mcpConfigDisposition({
        extension: 'mcp',
        meta: { ...meta, status: 'committed' },
        receipt: { action: 'configured', name: null, generation: '4' },
      })
    ).toEqual({
      status: 'pending',
      message: 'MCP config generation 4 committed; runtime settlement is pending',
    });
  });

  it('does not present a degraded Plugin settlement as success', async () => {
    bridge.apiInvoke.mockResolvedValue({
      extension: 'plugins',
      meta: { ...meta, status: 'degraded' },
      receipt: {
        action: 'mutation',
        projection: {
          plugin_id: 'formatter',
          plugin: null,
          status: 'degraded',
          target_receipts: {
            items: [
              {
                target: 'workspace-a',
                workspace_generation: 'workspace-generation-a',
                previous_prepared_generation: 'prepared-1',
                candidate_prepared_generation: 'prepared-2',
                status: 'degraded',
                diagnostics: { items: ['hook fanout failed'], omitted: 0 },
              },
            ],
            omitted: 0,
          },
          summary: {
            total: 1,
            enabled: 1,
            skills_loaded: 0,
            hooks_registered: 0,
            mcp_connected: 0,
            agents_loaded: 0,
            lsp_languages_loaded: 0,
            monitors_loaded: 0,
            themes_loaded: 0,
            output_styles_loaded: 0,
            errors: { items: ['hook fanout failed'], omitted: 0 },
          },
          active_theme: null,
          themes: { items: [], omitted: 0 },
          active_output_style: null,
          output_styles: { items: [], omitted: 0 },
        },
      },
    } satisfies ExtensionCommandReceipt);

    await expect(pluginApi.enable(requestScope, 'formatter')).resolves.toMatchObject({
      success: false,
      wiring_ok: false,
      errors: ['hook fanout failed'],
      errors_omitted: 0,
      settlement_status: 'degraded',
      target_receipts: {
        items: [
          expect.objectContaining({
            target: 'workspace-a',
            previous_prepared_generation: 'prepared-1',
            candidate_prepared_generation: 'prepared-2',
            status: 'degraded',
          }),
        ],
        omitted: 0,
      },
      meta: { ...meta, status: 'degraded' },
      projection: { plugin_id: 'formatter' },
    });
  });

  it('projects Hook and LSP receipts without JSON-shaped success envelopes', async () => {
    bridge.apiInvoke
      .mockResolvedValueOnce({
        extension: 'hooks',
        meta,
        receipt: {
          action: 'listed',
          sources: {
            items: [{ source: 'workspace_config', rules: 2 }],
            omitted: 3,
          },
        },
      } satisfies ExtensionCommandReceipt)
      .mockResolvedValueOnce({
        extension: 'lsp',
        meta,
        receipt: { action: 'status', message: 'rust: running' },
      } satisfies ExtensionCommandReceipt);

    await expect(hooksApi.list(requestScope)).resolves.toMatchObject({
      sources: [
        {
          source: 'workspace_config',
          rule_count: 2,
          projection: { source: 'workspace_config', rules: 2 },
        },
      ],
      omitted: 3,
      meta,
      receipt: { extension: 'hooks' },
    });
    await expect(lspApi.control(requestScope, 'status')).resolves.toMatchObject({
      message: 'rust: running',
      meta,
      receipt: { extension: 'lsp' },
    });
  });
});
