import { renderToStaticMarkup } from 'react-dom/server';
import { describe, expect, it } from 'vitest';
import { ExecutionProcessGroup } from './ExecutionProcessGroup';

describe('ExecutionProcessGroup', () => {
  it('collapses the entire execution timeline after completion', () => {
    const markup = renderToStaticMarkup(
      <ExecutionProcessGroup completed>
        <div>思考和 Subagent 执行详情</div>
      </ExecutionProcessGroup>
    );

    expect(markup).toContain('aria-expanded="false"');
    expect(markup).toContain('执行过程');
    expect(markup).toContain('已完成');
    expect(markup).not.toContain('思考和 Subagent 执行详情');
  });

  it('keeps the execution timeline visible while the task is running', () => {
    const markup = renderToStaticMarkup(
      <ExecutionProcessGroup completed={false}>
        <div>实时执行详情</div>
      </ExecutionProcessGroup>
    );

    expect(markup).toContain('实时执行详情');
    expect(markup).not.toContain('已完成');
  });
});
