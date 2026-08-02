import { renderToStaticMarkup } from 'react-dom/server';
import { describe, expect, it } from 'vitest';
import { ThinkingSegment } from './ThinkingSegment';

describe('ThinkingSegment', () => {
  it('starts collapsed in completed execution history', () => {
    const markup = renderToStaticMarkup(
      <ThinkingSegment index={1} total={1} content="内部思考详情" isStreaming={false} />
    );

    expect(markup).toContain('aria-expanded="false"');
    expect(markup).not.toContain('内部思考详情');
  });

  it('stays visible while execution is streaming', () => {
    const markup = renderToStaticMarkup(
      <ThinkingSegment index={1} total={1} content="正在思考" isStreaming />
    );

    expect(markup).toContain('aria-expanded="true"');
    expect(markup).toContain('正在思考');
  });
});
