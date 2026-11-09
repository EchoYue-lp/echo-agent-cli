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

it('uses neutral history knowledge instead of a completed success mark for unrestored tools', () => {
  const markup = renderToStaticMarkup(
    <ExecutionProcessGroup completed unrestored>
      <span>未知工具原记录</span>
    </ExecutionProcessGroup>
  );
  expect(markup).toContain('历史执行状态未恢复');
  expect(markup).not.toContain('已完成');
  expect(markup).not.toContain('lucide-check');
  expect(markup).not.toContain('animate-spin');
});

it('keeps the process settlement and makes known tool failures visible without a success icon', () => {
  const markup = renderToStaticMarkup(
    <ExecutionProcessGroup completed toolFailureCount={2}>
      <span>收起的工具详情</span>
    </ExecutionProcessGroup>
  );
  expect(markup).toContain('2');
  expect(markup).toContain('个工具失败');
  expect(markup).toContain('已完成');
  expect(markup).not.toContain('lucide-check');
});
