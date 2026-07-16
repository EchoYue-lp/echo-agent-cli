import { describe, expect, it } from 'vitest';
import type { ToolExecution } from '../types/api';
import { finalToolProjection } from './conversationStore';

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
});
