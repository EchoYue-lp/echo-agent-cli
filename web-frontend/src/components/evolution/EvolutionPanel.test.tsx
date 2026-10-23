// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const api = vi.hoisted(() => ({
  dashboard: vi.fn(),
  scanProposals: vi.fn(),
  scanSkillCandidates: vi.fn(),
  curator: vi.fn(),
  listEvidence: vi.fn(),
  review: vi.fn(),
}));

vi.mock('../../api/endpoints', () => ({ evolutionApi: api }));

import { EvolutionPanel } from './EvolutionPanel';

describe('EvolutionPanel run review settlement', () => {
  afterEach(cleanup);

  beforeEach(() => {
    for (const mock of Object.values(api)) mock.mockReset();
    api.dashboard.mockResolvedValue({ metrics: null });
    api.scanProposals.mockResolvedValue({ proposals: [] });
    api.scanSkillCandidates.mockResolvedValue({ candidates: [] });
    api.curator.mockResolvedValue({ status: null });
    api.listEvidence.mockResolvedValue({ candidates: [] });
  });

  it.each([
    { persisted: null, label: '保存状态待确认' },
    { persisted: false, label: '未自动保存' },
    { persisted: true, label: '已保存为草稿记忆' },
  ])(
    'keeps the candidate and $label visible alongside a review error',
    async ({ persisted, label }) => {
      api.review.mockResolvedValue({
        success: false,
        review_operation_id: 'operation-review-partial',
        run_id: 'run-review-partial',
        actions: ['Proposed an evidence-linked candidate'],
        nothing_to_save: false,
        candidate: {
          kind: 'debugging_lesson',
          content: 'A cleanup error does not discard a supported candidate.',
          evidence: 'The user confirmed the successful work before cleanup failed.',
          confidence: 0.91,
          persisted,
        },
        evidence_candidate: null,
        error: 'Memory write completion is uncertain',
      });
      render(<EvolutionPanel />);

      fireEvent.click(screen.getByRole('button', { name: '立即审查' }));

      expect(
        await screen.findByText('A cleanup error does not discard a supported candidate.')
      ).toBeTruthy();
      expect(screen.getByText(/The user confirmed the successful work/)).toBeTruthy();
      expect(screen.getByText('Proposed an evidence-linked candidate')).toBeTruthy();
      expect(screen.getByText(new RegExp(label))).toBeTruthy();
      expect(screen.getByText(/Memory write completion is uncertain/)).toBeTruthy();
      expect(screen.getByText(/operation-review-partial/)).toBeTruthy();
      expect(screen.getByText('未生成 Review Inbox 候选。')).toBeTruthy();
      expect(screen.queryByText(/已加入 Review Inbox/)).toBeNull();
    }
  );

  it('reports an inbox candidate only from the durable settlement receipt', async () => {
    api.review.mockResolvedValue({
      success: false,
      review_operation_id: 'operation-inbox',
      run_id: 'run-inbox',
      actions: ['Candidate ready for review'],
      nothing_to_save: false,
      candidate: {
        kind: 'project_fact',
        content: 'Review Inbox and draft-memory persistence are independent.',
        evidence: 'The durable owner recorded an inbox candidate.',
        confidence: 0.8,
        persisted: false,
      },
      evidence_candidate: { candidate_id: 'evidence-durable-id' },
      error: 'Hot-memory projection remains pending',
    });
    render(<EvolutionPanel />);

    fireEvent.click(screen.getByRole('button', { name: '立即审查' }));

    expect(await screen.findByText(/已加入 Review Inbox: evidence-durable-id/)).toBeTruthy();
    expect(screen.getByText(/未自动保存/)).toBeTruthy();
    expect(screen.getByText(/Hot-memory projection remains pending/)).toBeTruthy();
    expect(screen.queryByText('未生成 Review Inbox 候选。')).toBeNull();
  });

  it('retains an error-only operation receipt without inventing a candidate', async () => {
    api.review.mockResolvedValue({
      success: false,
      review_operation_id: 'operation-failed',
      run_id: 'run-failed',
      actions: [],
      nothing_to_save: true,
      candidate: null,
      evidence_candidate: null,
      error: 'Review provider failed',
    });
    render(<EvolutionPanel />);

    fireEvent.click(screen.getByRole('button', { name: '立即审查' }));

    expect(await screen.findByText(/Review provider failed/)).toBeTruthy();
    expect(screen.getByText(/operation-failed/)).toBeTruthy();
    expect(screen.queryByText(/已加入 Review Inbox/)).toBeNull();
    expect(screen.queryByText(/置信度:/)).toBeNull();
  });
});
