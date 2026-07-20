import { renderToStaticMarkup } from 'react-dom/server';
import { describe, expect, it } from 'vitest';
import { InlineToolCall } from './InlineToolCall';

describe('InlineToolCall', () => {
  it('keeps the read tool name visible before a long path', () => {
    const html = renderToStaticMarkup(
      <InlineToolCall
        index={0}
        toolCall={{
          id: 'call-read',
          name: 'read_file',
          args: { path: './echo-agent-app-core/src/tasks/task_runtime/types.rs', offset: 1 },
          result: 'content',
          success: true,
          status: 'succeeded',
          stdout: 'content',
          stderr: '',
          log: '',
          startedAt: 1,
          finishedAt: 2,
        }}
      />
    );

    expect(html).toContain('Read ./echo-agent-app-core/src/tasks/task_runtime/types.rs');
    expect(html).toContain('· from line 1');
    expect(html).toContain('from line 1');
  });

  it('does not render tool output while collapsed', () => {
    const html = renderToStaticMarkup(
      <InlineToolCall
        index={0}
        toolCall={{
          id: 'call-1',
          name: 'shell',
          args: { command: 'printf visible-title' },
          result: 'hidden result',
          success: true,
          status: 'succeeded',
          stdout: 'hidden stdout',
          stderr: '',
          log: '',
          startedAt: 1,
          finishedAt: 2,
        }}
      />
    );

    expect(html).toContain('Shell printf visible-title');
    expect(html).not.toContain('hidden result');
    expect(html).not.toContain('hidden stdout');
  });

  it('keeps plan_create collapsed to a concise one-line summary', () => {
    const html = renderToStaticMarkup(
      <InlineToolCall
        index={0}
        toolCall={{
          id: 'call-plan',
          name: 'plan_create',
          args: {
            title: 'Core 库模块架构分析',
            description: 'Long plan description',
            allowed_tools: ['read_file', 'glob', 'code_search', 'grep', 'repo_map'],
          },
          result: 'created',
          success: true,
          status: 'succeeded',
          stdout: '',
          stderr: '',
          log: '',
          startedAt: 1,
          finishedAt: 2,
        }}
      />
    );

    expect(html).toContain('plan_create');
    expect(html).toContain('Core 库模块架构分析');
    expect(html).toContain('whitespace-nowrap');
    expect(html).not.toContain('allowed_tools');
  });

  it('renders the durable artifact entry independently from tool success', () => {
    const html = renderToStaticMarkup(
      <InlineToolCall
        index={0}
        toolCall={{
          id: 'call-artifact',
          name: 'shell',
          args: { command: 'large-output' },
          result: '',
          success: true,
          status: 'succeeded',
          stdout: 'bounded preview',
          stderr: '',
          log: '',
          startedAt: 1,
          finishedAt: 2,
          truncated: true,
          metadata: {
            artifact_path: '/tmp/tool.log',
            artifact_bytes: String(10 * 1024 * 1024),
            artifact_sha256: 'abcdef0123456789',
          },
        }}
      />
    );

    expect(html).toContain('large-output');
    expect(html).toContain('打开完整日志 artifact');
  });
});
