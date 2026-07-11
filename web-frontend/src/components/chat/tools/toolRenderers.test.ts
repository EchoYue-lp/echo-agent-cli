import { describe, expect, it } from 'vitest';
import type { ToolExecution } from '../../../types/api';
import { describeToolExecution } from './toolRenderers';

function tool(name: string, args: unknown, result = ''): ToolExecution {
  return {
    id: `call-${name}`,
    name,
    args,
    result,
    success: true,
    status: 'succeeded',
    stdout: result,
    stderr: '',
    log: '',
    startedAt: 1,
    finishedAt: 2,
  };
}

describe('tool renderer registry', () => {
  it('describes read ranges without exposing JSON args', () => {
    expect(
      describeToolExecution(tool('read_file', { path: 'src/main.rs', offset: 10, limit: 20 }))
    ).toMatchObject({ kind: 'read', title: 'src/main.rs', detail: 'lines 10-29' });
  });

  it('summarizes file writes with path and line count', () => {
    expect(
      describeToolExecution(tool('write_file', { path: 'a.ts', content: 'one\ntwo' }))
    ).toMatchObject({ kind: 'write', title: 'Write a.ts', detail: '2 lines' });
  });

  it('summarizes search scope and result count', () => {
    expect(
      describeToolExecution(
        tool('grep', { pattern: 'ToolResult', path: 'src' }, '12 matches found')
      )
    ).toMatchObject({
      kind: 'search',
      title: 'Search “ToolResult”',
      detail: 'in src · 12 matches',
    });
  });

  it('falls back to a generic descriptor for unknown tools', () => {
    expect(describeToolExecution(tool('custom_tool', { value: 1 }))).toMatchObject({
      kind: 'generic',
      title: 'custom_tool',
      detail: '{"value":1}',
    });
  });
});
