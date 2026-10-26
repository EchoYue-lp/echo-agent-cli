/// <reference types="node" />
// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import type { ManagedChannelStatus } from './endpoints';

const native = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/core', () => ({ invoke: native.invoke }));
const rustChannels = readFileSync(
  resolve(dirname(fileURLToPath(import.meta.url)), '../../../src/tauri/commands/channels.rs'),
  'utf8'
);

// Check the actual producer, not a second handwritten native schema. Every
// endpoint below traverses the real apiInvoke/tauri-bridge to this native boundary.
const commands = new Map(
  [
    ...rustChannels.matchAll(
      /#\[tauri::command(?:\(([^)]*)\))?\]\s*pub async fn (\w+)\(([\s\S]*?)\)\s*->/g
    ),
  ].map((match) => [match[2], { annotation: match[1] ?? '', signature: match[3] }] as const)
);

function checkNativeArguments(command: string, args: Record<string, unknown> = {}) {
  const producer = commands.get(command);
  expect(producer, `Actual channels.rs producer for ${command}`).toBeDefined();
  if (!producer) throw new Error(`Missing Rust command ${command}`);
  expect(producer.annotation.replace(/\s/g, '')).toBe('rename_all="snake_case"');
  const parameters = [...producer.signature.matchAll(/^\s*(\w+):\s*(.+),\s*$/gm)]
    .filter((match) => !match[2].includes('tauri::State'))
    .map((match) => ({ name: match[1], optional: match[2].startsWith('Option<') }));
  // Tauri serializes undefined away, while explicit null must reach Option fields.
  const wire = JSON.parse(JSON.stringify(args)) as Record<string, unknown>;
  expect(Object.keys(wire).every((key) => parameters.some((field) => field.name === key))).toBe(
    true
  );
  for (const parameter of parameters) {
    if (!parameter.optional) expect(wire).toHaveProperty(parameter.name);
  }
}

beforeEach(() => {
  vi.resetModules();
  native.invoke.mockReset();
  window.__TAURI_INTERNALS__ = {};
  native.invoke.mockImplementation(async (command: string, args?: Record<string, unknown>) => {
    checkNativeArguments(command, args);
    return { original_native_receipt: command };
  });
});
afterEach(() => {
  delete window.__TAURI_INTERNALS__;
});

describe('real Weixin endpoints through the native invoke boundary', () => {
  it('keeps every channel command explicitly bound to the snake_case frontend contract', () => {
    expect(commands.size).toBe(12);
    for (const producer of commands.values())
      expect(producer.annotation.replace(/\s/g, '')).toBe('rename_all="snake_case"');
  });

  it('passes authorization revision, exact challenge and unmodified numeric code to Rust', async () => {
    const { weixinQrApi } = await import('./endpoints');
    await expect(weixinQrApi.begin('revision-original')).resolves.toEqual({
      original_native_receipt: 'begin_weixin_login',
    });
    await weixinQrApi.status('challenge/specific:2');
    await weixinQrApi.verify('challenge/specific:2', '004812');
    await weixinQrApi.cancel('challenge/specific:2');
    await weixinQrApi.forget('revision-original');
    expect(native.invoke.mock.calls).toEqual([
      ['begin_weixin_login', { expected_revision: 'revision-original' }],
      ['get_weixin_login_status', { challenge_id: 'challenge/specific:2' }],
      ['verify_weixin_login', { challenge_id: 'challenge/specific:2', code: '004812' }],
      ['cancel_weixin_login', { challenge_id: 'challenge/specific:2' }],
      ['forget_weixin_login', { expected_revision: 'revision-original' }],
    ]);
  });

  it('keeps an omitted status challenge optional without replacing a specified one', async () => {
    const { weixinQrApi } = await import('./endpoints');
    await weixinQrApi.status();
    await weixinQrApi.status('exact-challenge');
    expect(native.invoke.mock.calls).toEqual([
      ['get_weixin_login_status', { challenge_id: undefined }],
      ['get_weixin_login_status', { challenge_id: 'exact-challenge' }],
    ]);
  });

  it('passes original task, definition, revision and run identity for notifications', async () => {
    const { weixinNotificationApi } = await import('./endpoints');
    await weixinNotificationApi.configuration('definition-original');
    await weixinNotificationApi.configure(
      'task-original',
      'definition-original',
      'revision-2',
      true
    );
    await weixinNotificationApi.configure(
      'task-original',
      'definition-original',
      'revision-3',
      false
    );
    await weixinNotificationApi.result('run-original');
    expect(native.invoke.mock.calls).toEqual([
      ['get_weixin_notification_configuration', { definition_id: 'definition-original' }],
      [
        'configure_weixin_notification',
        {
          task_id: 'task-original',
          definition_id: 'definition-original',
          expected_revision: 'revision-2',
          enabled: true,
        },
      ],
      [
        'configure_weixin_notification',
        {
          task_id: 'task-original',
          definition_id: 'definition-original',
          expected_revision: 'revision-3',
          enabled: false,
        },
      ],
      ['get_weixin_notification_result', { run_id: 'run-original' }],
    ]);
  });

  it.each([
    ['weixin', '18446744073709551615', 'applied-old', 'stop'],
    ['qqbot', null, null, 'start'],
    ['feishu', '0', null, 'reconnect'],
  ] as const)(
    'preserves %s original control revision and nullable instance fields',
    async (channel, epoch, applied, action) => {
      const { channelApi } = await import('./endpoints');
      const instance: ManagedChannelStatus = {
        id: channel,
        registered: true,
        configuration_revision: 'desired-new',
        applied_configuration_revision: applied,
        mutation_epoch: epoch,
        completed_cleanup_failures: [],
        handler_attached: true,
        active_invocations: 0,
        shutdown_settled: false,
        pump_shutdown_settled: false,
        transport: { phase: 'unknown', evidence: null, error: null, observed_at: null },
      };
      await channelApi.control(channel, 'desired-new', instance, action);
      expect(native.invoke).toHaveBeenCalledExactlyOnceWith('control_channel', {
        channel,
        expected_revision: 'desired-new',
        expected_mutation_epoch: epoch,
        expected_applied_revision: applied,
        action,
      });
    }
  );

  it('retains nested patch serde names while ordinary reads carry no authentication payload', async () => {
    const { channelApi } = await import('./endpoints');
    const patch = {
      channel: 'feishu' as const,
      expected_revision: 'patch-revision',
      enabled: true,
      app_id: 'original-app',
      secret: { action: 'preserve' as const },
      verification_token: { action: 'clear' as const },
    };
    await channelApi.configuration();
    await channelApi.status();
    await channelApi.save(patch);
    expect(native.invoke.mock.calls).toEqual([
      ['get_channel_configuration', undefined],
      ['list_channel_status', undefined],
      ['save_channel_configuration', { patch }],
    ]);
  });
});
