/// <reference types="node" />
// @vitest-environment jsdom
import { readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { SchedulerTask } from './endpoints';

const native = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/core', () => ({ invoke: native.invoke }));

const rustScheduler = readFileSync(
  resolve(dirname(fileURLToPath(import.meta.url)), '../../../src/tauri/commands/scheduler.rs'),
  'utf8'
);
const commands = new Map(
  [
    ...rustScheduler.matchAll(
      /#\[tauri::command(?:\(([^)]*)\))?\]\s*pub async fn (\w+)\(([\s\S]*?)\)\s*->/g
    ),
  ].map((match) => [match[2], { annotation: match[1] ?? '', signature: match[3] }] as const)
);

// 独立依据：锁定的 tauri-macros 2.7.3 wrapper 默认 ArgumentCase::Camel，
// parse_arg 再用 to_lower_camel_case；Tauri 2.12.4 ipc/command.rs 按该 key 取 JSON。
// 这里只在原生边界模拟取参；endpoints、apiInvoke、bridge 与 HTTP serializer 都走真实实现。
function decodeNativeArguments(command: string, args: Record<string, unknown> = {}) {
  const producer = commands.get(command);
  if (!producer) throw new Error(`Missing actual scheduler command ${command}`);
  expect(producer.annotation).toBe('');
  const parameters = [...producer.signature.matchAll(/^\s*(\w+):\s*(.+),\s*$/gm)]
    .filter((match) => !match[2].includes('tauri::State'))
    .map((match) => ({
      key: match[1].replace(/_([a-z])/g, (_, letter: string) => letter.toUpperCase()),
      type: match[2],
      optional: match[2].startsWith('Option<'),
    }));
  const wire = JSON.parse(JSON.stringify(args)) as Record<string, unknown>;
  for (const parameter of parameters) {
    if (!parameter.optional && !Object.hasOwn(wire, parameter.key)) {
      throw new Error(`command ${command} missing required key ${parameter.key}`);
    }
    const value = wire[parameter.key];
    if (parameter.type.includes('String') && value != null && typeof value !== 'string') {
      throw new Error(`command ${command} expected string for ${parameter.key}`);
    }
  }
  return wire;
}

const task: SchedulerTask = {
  id: 'original-task',
  definition_id: 'original-definition',
  control_revision: 7,
  name: '验证cron历史',
  cron_expr: '0 0 1 1 *',
  prompt: 'NATIVE_HELLO',
  status: 'disabled',
  last_run_at: null,
  last_result: null,
  created_at: '2026-11-01T00:00:00Z',
  next_run: null,
};
const edit = { name: task.name, cron_expr: task.cron_expr, prompt: task.prompt };
const cursor =
  '{"after_sequence":18446744073709551614,"snapshot_end_sequence":18446744073709551615}';

beforeEach(() => {
  vi.resetModules();
  native.invoke.mockReset();
  sessionStorage.clear();
  window.__TAURI_INTERNALS__ = {};
  native.invoke.mockImplementation(async (command: string, args?: Record<string, unknown>) => {
    decodeNativeArguments(command, args);
    return { original_native_receipt: command };
  });
});
afterEach(() => {
  delete window.__TAURI_INTERNALS__;
  vi.unstubAllGlobals();
});

describe('scheduler endpoints at the actual bridge boundary', () => {
  it('tracks the seven current Rust commands using default Tauri argument naming', () => {
    expect(commands.size).toBe(7);
    for (const producer of commands.values()) expect(producer.annotation).toBe('');
  });

  it('creates the reported task with the top-level cronExpr required by the Rust command', async () => {
    const { schedulerApi } = await import('./endpoints');
    await expect(schedulerApi.create(edit)).resolves.toEqual({
      original_native_receipt: 'add_scheduler_task',
    });
    expect(native.invoke).toHaveBeenCalledExactlyOnceWith('add_scheduler_task', {
      name: task.name,
      cronExpr: task.cron_expr,
      prompt: task.prompt,
    });
  });

  it('rejects the original snake_case top-level key before the native handler body', () => {
    expect(() => decodeNativeArguments('add_scheduler_task', edit)).toThrow(
      'missing required key cronExpr'
    );
  });

  it('keeps edit payloads in their original serde shape under unchanged top-level arguments', async () => {
    const { schedulerApi } = await import('./endpoints');
    await schedulerApi.edit(task, edit);
    expect(native.invoke).toHaveBeenCalledExactlyOnceWith('edit_scheduler_task', {
      expected: task,
      edit,
    });
    expect(task.cron_expr).toBe('0 0 1 1 *');
    expect(edit).not.toHaveProperty('cronExpr');
  });

  it('preserves list, status, run and delete argument names and original receipts', async () => {
    const { schedulerApi } = await import('./endpoints');
    await schedulerApi.list();
    await schedulerApi.updateStatus(task.id, false);
    await schedulerApi.updateStatus(task.id, true);
    await expect(schedulerApi.run(task.id)).resolves.toEqual({
      original_native_receipt: 'run_scheduler_task',
    });
    await schedulerApi.delete(task.id);
    expect(native.invoke.mock.calls).toEqual([
      ['list_scheduler_tasks', undefined],
      ['set_scheduler_task_status', { id: task.id, status: 'disabled' }],
      ['set_scheduler_task_status', { id: task.id, status: 'enabled' }],
      ['run_scheduler_task', { id: task.id }],
      ['remove_scheduler_task', { id: task.id }],
    ]);
  });

  it('passes opaque history cursor bytes unchanged, including u64 values and null', async () => {
    const { schedulerApi } = await import('./endpoints');
    await schedulerApi.history();
    await schedulerApi.history(cursor, 9);
    expect(native.invoke.mock.calls).toEqual([
      ['read_scheduler_history', { cursor: null, limit: 128 }],
      ['read_scheduler_history', { cursor, limit: 9 }],
    ]);
  });

  it('rejects an object cursor at the actual Rust String argument boundary', () => {
    expect(() => decodeNativeArguments('read_scheduler_history', { cursor: {}, limit: 9 })).toThrow(
      'expected string for cursor'
    );
  });
});

describe('scheduler REST serialization remains independent of Tauri naming', () => {
  const fetch = vi.fn();
  beforeEach(() => {
    delete window.__TAURI_INTERNALS__;
    fetch
      .mockReset()
      .mockResolvedValue({ ok: true, status: 200, json: async () => ({ success: true }) });
    vi.stubGlobal('fetch', fetch);
  });

  it('keeps create cron_expr in the real HTTP client JSON body', async () => {
    const { schedulerApi } = await import('./endpoints');
    await schedulerApi.create(edit);
    expect(fetch).toHaveBeenCalledExactlyOnceWith('/api/scheduler/tasks', {
      headers: { 'Content-Type': 'application/json' },
      method: 'POST',
      body: JSON.stringify(edit),
    });
    expect(native.invoke).not.toHaveBeenCalled();
  });

  it('preserves nested edit and opaque cursor serialization on REST', async () => {
    const { schedulerApi } = await import('./endpoints');
    await schedulerApi.edit(task, edit);
    await schedulerApi.history(cursor, 9);
    expect(fetch.mock.calls).toEqual([
      [
        '/api/scheduler/tasks/original-task',
        {
          headers: { 'Content-Type': 'application/json' },
          method: 'PUT',
          body: JSON.stringify({ expected: task, edit }),
          signal: undefined,
        },
      ],
      [
        '/api/scheduler/history',
        {
          headers: { 'Content-Type': 'application/json' },
          method: 'POST',
          body: JSON.stringify({ cursor, limit: 9 }),
        },
      ],
    ]);
    expect(native.invoke).not.toHaveBeenCalled();
  });
});
