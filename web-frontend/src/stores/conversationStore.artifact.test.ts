import { describe, expect, it } from 'vitest';
import type { ToolExecution } from '../types/api';
import { finalToolProjection, mergeRestoredToolCalls } from './conversationStore';

describe('conversation tool artifact projection', () => {
  it('keeps a bounded preview and preserves the durable artifact reference', () => {
    const tool: ToolExecution = {
      id: 'call-10mb',
      name: 'shell',
      args: { command: 'large-output' },
      result: '',
      success: true,
      status: 'succeeded',
      stdout: 'x'.repeat(10 * 1024 * 1024),
      stderr: '',
      log: '',
      startedAt: 1,
      finishedAt: 2,
      metadata: {
        artifact_path: '/tmp/tool.log',
        artifact_bytes: String(10 * 1024 * 1024),
        artifact_sha256: 'abcdef0123456789',
        artifact_retention: 'conversation_or_30d',
      },
    };

    const projected = finalToolProjection(tool);

    expect(projected.stdout.length).toBeLessThan(10 * 1024 * 1024);
    expect(projected.truncated).toBe(true);
    expect(projected.metadata).toEqual(tool.metadata);
  });

  it('restores plan_create calls omitted from an incomplete execution round', () => {
    const existing: ToolExecution = {
      id: 'call-read',
      name: 'read_file',
      args: { path: 'README.md' },
      result: 'read',
      success: true,
      status: 'succeeded',
      stdout: 'read',
      stderr: '',
      log: '',
      startedAt: 1,
      finishedAt: 2,
    };

    const restored = mergeRestoredToolCalls(
      [existing],
      [
        { id: 'call-read', name: 'read_file', arguments: '{"path":"README.md"}' },
        {
          id: 'call-plan',
          name: 'plan_create',
          arguments: '{"title":"Core 库模块架构分析","description":"Long"}',
        },
      ],
      10
    );

    expect(restored).toHaveLength(2);
    expect(restored[0]).toBe(existing);
    expect(restored[1]).toMatchObject({
      id: 'call-plan',
      name: 'plan_create',
      args: { title: 'Core 库模块架构分析', description: 'Long' },
      startedAt: 10,
    });
  });
});
