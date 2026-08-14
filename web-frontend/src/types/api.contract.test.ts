import { describe, expect, expectTypeOf, it } from 'vitest';
import type { RuntimeEventKind, StreamingEvent, ToolInfo } from '../generated';
import type { TauriMcpServerInfo, TauriSkillInfo } from './api';

const serializedToolInfo = {
  name: 'read_file',
  description: 'Read a file',
  parameters: { type: 'object', properties: { path: { type: 'string' } } },
  enabled: true,
  need_approval: false,
  source: 'Builtin',
} satisfies ToolInfo;

const serializedMcpServer = {
  name: 'local-tools',
  status: 'disconnected',
  transport: 'stdio',
  tool_count: 0,
  tools: [],
  connected_at: null,
  error: null,
  enabled: true,
} satisfies TauriMcpServerInfo;

const serializedSkill = {
  name: 'research',
  description: 'Research workflow',
  file: '/skills/research/SKILL.md',
  loaded: true,
  source: 'builtin',
  version: null,
  author: null,
  upstream_version: null,
} satisfies TauriSkillInfo;

const streamingEvents = [
  { event: 'token', data: 'hello' },
  { event: 'tool_batch_start', tool_count: 2 },
  { event: 'cancelled' },
  { event: 'done' },
] satisfies StreamingEvent[];

const runtimeEventVariants = [
  'run_started',
  'task_completed',
  'subagent_assigned',
  'artifact_produced',
  'run_cancelled',
] satisfies RuntimeEventKind[];

describe('Rust serialization contracts', () => {
  it('consumes the generated ToolInfo wire fields', () => {
    expect(serializedToolInfo.parameters).toHaveProperty('properties.path');
    expect(serializedToolInfo.need_approval).toBe(false);
    expectTypeOf(serializedToolInfo).toMatchTypeOf<ToolInfo>();
  });

  it('preserves explicit nulls from Tauri projections', () => {
    expect(serializedMcpServer.connected_at).toBeNull();
    expect(serializedMcpServer.error).toBeNull();
    expect(serializedSkill.version).toBeNull();
  });

  it('keeps representative streaming and runtime event variants typed', () => {
    expect(streamingEvents.map((event) => event.event)).toEqual([
      'token',
      'tool_batch_start',
      'cancelled',
      'done',
    ]);
    expect(runtimeEventVariants).toContain('artifact_produced');
  });
});
