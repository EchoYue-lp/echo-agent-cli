// @vitest-environment jsdom
import { act, cleanup, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { Result } from 'vega-embed';
const mocks = vi.hoisted(() => ({ embed: vi.fn(), interpreter: { controlled: true } }));
vi.mock('vega-embed', () => ({ default: mocks.embed }));
vi.mock('vega-interpreter', () => ({ expressionInterpreter: mocks.interpreter }));
import { ChartCard } from './ChartCard';

beforeEach(() => {
  vi.clearAllMocks();
});
afterEach(cleanup);

describe('本地官方图表生命周期', () => {
  it('同一提交立即卸载时不再启动图表模块', async () => {
    const { unmount } = render(<ChartCard spec={{ mark: 'bar' }} />);
    unmount();
    await act(async () => {
      await Promise.resolve();
    });
    expect(mocks.embed).not.toHaveBeenCalled();
  });
  it('原spec使用SVG/AST/官方解释器，清理既有view且不创建iframe或远端脚本', async () => {
    const finalize = vi.fn();
    mocks.embed.mockResolvedValue({ finalize });
    const spec = {
      $schema: 'https://vega.github.io/schema/vega-lite/v5.json',
      mark: 'bar',
      data: { values: [{ x: 1, y: 7 }] },
    };
    const original = JSON.stringify(spec);
    const { unmount } = render(<ChartCard spec={spec} />);
    await waitFor(() => expect(mocks.embed).toHaveBeenCalledTimes(1));
    expect(mocks.embed.mock.calls.at(0)?.at(1)).toBe(spec);
    expect(mocks.embed.mock.calls.at(0)?.at(2)).toMatchObject({
      renderer: 'svg',
      ast: true,
      expr: mocks.interpreter,
    });
    await waitFor(() =>
      expect(document.querySelector('[data-chart-state="ready"]')).not.toBeNull()
    );
    expect(document.querySelector('iframe,script[src]')).toBeNull();
    expect(JSON.stringify(spec)).toBe(original);
    unmount();
    expect(finalize).toHaveBeenCalledTimes(1);
  });

  it('晚到的旧spec结果只销毁自己，不能污染新surface', async () => {
    let first: ((result: Result) => void) | undefined;
    const firstFinalize = vi.fn();
    const secondFinalize = vi.fn();
    mocks.embed
      .mockImplementationOnce(
        () =>
          new Promise<Result>((resolve) => {
            first = resolve;
          })
      )
      .mockResolvedValueOnce({ finalize: secondFinalize });
    const { rerender, unmount } = render(<ChartCard spec={{ mark: 'bar' }} />);
    await waitFor(() => expect(mocks.embed).toHaveBeenCalledTimes(1));
    const oldSurface = mocks.embed.mock.calls.at(0)?.at(0) as HTMLElement;
    rerender(<ChartCard spec={{ mark: 'line' }} />);
    await waitFor(() => expect(mocks.embed).toHaveBeenCalledTimes(2));
    await act(async () => {
      first?.({ finalize: firstFinalize } as unknown as Result);
    });
    expect(firstFinalize).toHaveBeenCalledTimes(1);
    expect(oldSurface.isConnected).toBe(false);
    expect(secondFinalize).not.toHaveBeenCalled();
    unmount();
    expect(secondFinalize).toHaveBeenCalledTimes(1);
  });

  it('真实SDK拒绝时保留原数据并展示失败，不伪报图形已绘制', async () => {
    mocks.embed.mockRejectedValue(new Error('controlled SDK rejection'));
    render(<ChartCard spec={{ source: '原数据保留' }} />);
    await waitFor(() =>
      expect(screen.getByRole('status').textContent).toContain('controlled SDK rejection')
    );
    expect(screen.getByText(/原数据保留/)).not.toBeNull();
    expect(document.querySelector('[data-chart-state="error"]')).not.toBeNull();
  });
});
