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

  it('summarizes browser actions by domain and target', () => {
    const execution = tool('browser_click', { target: 'Search' });
    execution.metadata = {
      browser_url: 'https://docs.rs/echo-agent',
      browser_title: 'echo-agent docs',
    };
    expect(describeToolExecution(execution)).toMatchObject({
      kind: 'browser',
      title: 'Click',
      detail: 'echo-agent docs · https://docs.rs/echo-agent · Search',
    });
  });

  it('summarizes MCP identity and structured result type', () => {
    expect(describeToolExecution(tool('mcp__github__list_issues', {}, '[{"id":1}]'))).toMatchObject(
      {
        kind: 'mcp',
        title: 'github · list_issues',
        detail: 'JSON array',
      }
    );
  });

  it('uses MCP result metadata from the framework adapter', () => {
    const execution = tool('list_issues', {}, '{"items":[]}');
    execution.metadata = {
      tool_source: 'mcp',
      mcp_server: 'github',
      mcp_tool: 'list_issues',
      result_type: 'json',
    };
    expect(describeToolExecution(execution)).toMatchObject({
      kind: 'mcp',
      title: 'github · list_issues',
      detail: 'JSON result',
    });
  });

  it('summarizes subagent dispatch without duplicating its execution panel', () => {
    expect(
      describeToolExecution(
        tool('agent_tool', { agent_name: 'reviewer', task: 'Review the browser renderer' })
      )
    ).toMatchObject({
      kind: 'task',
      title: 'Subagent reviewer',
      detail: 'Review the browser renderer',
    });
  });
});
