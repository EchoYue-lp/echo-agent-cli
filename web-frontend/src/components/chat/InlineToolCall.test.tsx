import { renderToStaticMarkup } from 'react-dom/server';
import { describe, expect, it } from 'vitest';
import { InlineToolCall } from './InlineToolCall';

describe('InlineToolCall', () => {
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

    expect(html).toContain('printf visible-title');
    expect(html).not.toContain('hidden result');
    expect(html).not.toContain('hidden stdout');
  });
});
