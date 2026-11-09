// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';
import { ConversationStatistics } from './ConversationStatistics';
import type { ConversationUsageSnapshot, UsageTotals } from '../../types/api';

const main: UsageTotals = {
  attempts: 3,
  steps: 2,
  input_tokens: 3500,
  output_tokens: 520,
  cached_tokens: 2300,
  cache_write_tokens: 69,
  missing_usage: 0,
  unidentified_steps: 0,
  conflicts: 0,
  pending_calls: 0,
  timed_output_tokens: 500,
  output_duration_ms: 3000,
  latest_speed: 200,
  latest_input_tokens: 500,
};
const value: ConversationUsageSnapshot = {
  workspace_id: 'global',
  conversation_id: 'conv',
  journal_identity: 'original',
  through_sequence: 5001,
  latest_sequence: 5001,
  first_observed_sequence: 1,
  prefix_complete: true,
  turns: 1,
  turns_known: true,
  main,
  subagent: { ...main, input_tokens: 40, output_tokens: 10 },
  models: { 'actual-model': main },
  auxiliary_coverage: 'unobserved',
};

describe('main-chain usage presentation', () => {
  afterEach(cleanup);
  it('keeps absent original journal evidence distinct from reported zero', () => {
    render(
      <ConversationStatistics
        streaming={false}
        value={{ ...value, journal_identity: null, prefix_complete: false, turns_known: false }}
        error={null}
      />
    );
    expect(screen.getByText('-- 轮 · -- 步')).not.toBeNull();
    expect(screen.getByLabelText('主链 -- tok')).not.toBeNull();
    expect(screen.getByText('覆盖不完整')).not.toBeNull();
  });
  it('shows recent speed, real cache weighting and separate weighted details', () => {
    render(<ConversationStatistics streaming value={value} error={null} />);
    expect(screen.getByText('1 轮 · 2 步')).not.toBeNull();
    expect(screen.getByText('上次 200 tok/s')).not.toBeNull();
    expect(screen.getByText('缓存 65.7%')).not.toBeNull();
    expect(screen.queryByText('覆盖不完整')).toBeNull();
    fireEvent.click(screen.getByText('1 轮 · 2 步'));
    expect(screen.getByText(/会话观测速率：167 tok\/s/)).not.toBeNull();
    expect(screen.getByText(/Subagent：输入 40 \/ 输出 10/)).not.toBeNull();
    expect(screen.getByText(/辅助调用.*未覆盖/)).not.toBeNull();
  });
  it('does not substitute zero or fabricate complete coverage on missing evidence', () => {
    render(
      <ConversationStatistics
        streaming={false}
        value={{
          ...value,
          prefix_complete: false,
          main: { ...main, latest_speed: null, missing_usage: 1, unidentified_steps: 1 },
        }}
        error={null}
      />
    );
    expect(screen.getByText('1 轮 · -- 步')).not.toBeNull();
    expect(screen.getByText('-- tok/s')).not.toBeNull();
    expect(screen.getByText('覆盖不完整')).not.toBeNull();
    expect(screen.getByText('缓存 65.7%')).not.toBeNull();
  });
});
