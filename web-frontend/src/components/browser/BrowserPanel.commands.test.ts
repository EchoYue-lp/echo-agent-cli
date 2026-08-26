// @vitest-environment jsdom

import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import { createElement } from 'react';
import { describe, expect, it, vi } from 'vitest';
import { BrowserToolShelf, browserToolCommand, type BrowserToolValues } from './BrowserPanel';

const values: BrowserToolValues = {
  target: 'button[ref=e1]',
  text: 'value',
  element: 'Save button',
  filename: 'artifacts/page.md',
  button: 'right',
  effect: 'publish',
  x: '12.5',
  y: '24',
  level: 'error',
  method: 'POST',
  status: '201',
  contains: '/api/',
  maxDepth: '6',
  traceAction: 'stop',
  path: 'artifacts/trace.zip',
  doubleClick: true,
  submit: true,
  slowly: true,
};

describe('BrowserPanel typed tool controls', () => {
  it('keeps every advanced Browser command reachable from the production tool shelf', async () => {
    const onExecute = vi.fn().mockResolvedValue({ status: 'settled', message: 'done' });
    render(
      createElement(BrowserToolShelf, {
        busy: false,
        developerMode: false,
        onExecute,
      })
    );

    const action = screen.getByRole('combobox', { name: '浏览器工具' });
    expect(Array.from((action as HTMLSelectElement).options).map((option) => option.value)).toEqual(
      [
        'status',
        'snapshot',
        'click_target',
        'fill',
        'type_at',
        'console',
        'network',
        'dom_inspect',
        'performance_trace',
      ]
    );

    fireEvent.change(action, { target: { value: 'fill' } });
    fireEvent.change(screen.getByRole('textbox', { name: '填写目标' }), {
      target: { value: 'input[ref=e2]' },
    });
    fireEvent.change(screen.getByRole('textbox', { name: '填写文本' }), {
      target: { value: 'typed value' },
    });
    fireEvent.click(screen.getByRole('button', { name: '执行' }));
    await waitFor(() =>
      expect(onExecute).toHaveBeenCalledWith({
        action: 'fill',
        target: 'input[ref=e2]',
        text: 'typed value',
        element: null,
        submit: false,
        slowly: false,
        effect: 'none',
      })
    );

    fireEvent.click(screen.getByRole('checkbox', { name: '开发者' }));
    await waitFor(() =>
      expect(onExecute).toHaveBeenLastCalledWith({ action: 'developer_mode', enabled: true })
    );
  });

  it('maps every advanced control into the generated Browser command union', () => {
    expect(browserToolCommand('status', values)).toEqual({ action: 'status' });
    expect(browserToolCommand('snapshot', values)).toEqual({
      action: 'snapshot',
      filename: 'artifacts/page.md',
    });
    expect(browserToolCommand('click_target', values)).toEqual({
      action: 'click_target',
      target: 'button[ref=e1]',
      element: 'Save button',
      button: 'right',
      double_click: true,
      effect: 'publish',
    });
    expect(browserToolCommand('fill', values)).toEqual({
      action: 'fill',
      target: 'button[ref=e1]',
      text: 'value',
      element: 'Save button',
      submit: true,
      slowly: true,
      effect: 'publish',
    });
    expect(browserToolCommand('type_at', values)).toEqual({
      action: 'type_at',
      x: 12.5,
      y: 24,
      text: 'value',
      submit: true,
      slowly: true,
      effect: 'publish',
    });
    expect(browserToolCommand('console', values)).toEqual({
      action: 'console',
      level: 'error',
      contains: '/api/',
    });
    expect(browserToolCommand('network', values)).toEqual({
      action: 'network',
      method: 'POST',
      status: 201,
      contains: '/api/',
    });
    expect(browserToolCommand('dom_inspect', values)).toEqual({
      action: 'dom_inspect',
      target: 'button[ref=e1]',
      text: 'value',
      max_depth: 6,
    });
    expect(browserToolCommand('performance_trace', values)).toEqual({
      action: 'performance_trace',
      trace_action: 'stop',
      path: 'artifacts/trace.zip',
    });
  });

  it('preserves optional values as null instead of inventing surface defaults', () => {
    const empty = Object.fromEntries(
      Object.entries(values).map(([key, value]) => [key, typeof value === 'boolean' ? false : ''])
    ) as unknown as BrowserToolValues;

    expect(browserToolCommand('snapshot', empty)).toEqual({ action: 'snapshot', filename: null });
    expect(browserToolCommand('network', empty)).toEqual({
      action: 'network',
      method: null,
      status: null,
      contains: null,
    });
    expect(browserToolCommand('dom_inspect', empty)).toEqual({
      action: 'dom_inspect',
      target: null,
      text: null,
      max_depth: null,
    });
  });
});
