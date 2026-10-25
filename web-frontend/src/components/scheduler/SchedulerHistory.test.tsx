// @vitest-environment jsdom
import { afterEach, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { SchedulerHistory } from './SchedulerHistory';
const history = vi.hoisted(() => vi.fn());
vi.mock('../../api/endpoints', () => ({ schedulerApi: { history } }));
afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});
it('preserves the original cursor and read facts on a retention failure without retry or reset', async () => {
  const token = '{"after_sequence":18446744073709551614}';
  history
    .mockResolvedValueOnce({
      success: true,
      page: {
        journal_identity: 'journal',
        retained_floor: '1',
        snapshot_end_sequence: '18446744073709551615',
        next_cursor: token,
        events: [
          {
            sequence: '1',
            batch_id: 'batch',
            event: {
              kind: 'persisted',
              envelope: {
                message_id: 'occurrence-original',
                payload: {
                  task: { id: 'task-a', name: '任务 A', definition_id: 'old-definition' },
                },
              },
            },
          },
        ],
      },
      runs: {},
    })
    .mockResolvedValueOnce({
      success: false,
      cause: 'retained_floor_changed',
      retained_floor: '10',
    });
  render(<SchedulerHistory />);
  fireEvent.click(screen.getByRole('button', { name: '读取运行历史' }));
  expect(await screen.findByText(/occurrence-original/)).toBeTruthy();
  expect(screen.getByText(/还有未读页/)).toBeTruthy();
  fireEvent.click(screen.getByRole('button', { name: '加载下一页' }));
  await waitFor(() => expect(history).toHaveBeenLastCalledWith(token));
  expect(await screen.findByRole('alert')).toBeTruthy();
  expect(screen.getByText(/occurrence-original/)).toBeTruthy();
  expect(history).toHaveBeenCalledTimes(2);
  expect(screen.getByText(/尚无可定位的 TaskRun/)).toBeTruthy();
});
