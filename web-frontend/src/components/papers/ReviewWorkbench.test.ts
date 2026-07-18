import { describe, expect, it } from 'vitest';
import type { SystematicReviewRecord } from '../../api/endpoints';
import { computePrismaFlow } from './ReviewWorkbench';

describe('computePrismaFlow', () => {
  it('derives screening and inclusion counts from review decisions', () => {
    const record = {
      source_ids: ['one', 'two'],
      screening: [
        {
          source_id: 'one',
          stage: 'title_abstract',
          decision: 'include',
          decided_at: '2026-07-18T00:00:00Z',
        },
        {
          source_id: 'two',
          stage: 'title_abstract',
          decision: 'exclude',
          decided_at: '2026-07-18T00:00:00Z',
        },
        {
          source_id: 'one',
          stage: 'full_text',
          decision: 'include',
          decided_at: '2026-07-18T00:00:00Z',
        },
      ],
      prisma: {
        additional_identified: 3,
        duplicates_removed: 1,
        reports_not_retrieved: 0,
      },
    } as SystematicReviewRecord;

    expect(computePrismaFlow(record)).toMatchObject({
      records_identified: 5,
      records_screened: 2,
      records_excluded: 1,
      reports_sought: 1,
      reports_assessed: 1,
      studies_included: 1,
    });
  });
});
