import { describe, expect, it } from 'vitest';
import { reorderById } from './queuedChat';

const items = [{ id: 'a' }, { id: 'b' }, { id: 'c' }];

describe('reorderById', () => {
  it('moves an earlier item after the drop target', () => {
    expect(reorderById(items, 'a', 'c').map((item) => item.id)).toEqual(['b', 'c', 'a']);
  });

  it('moves a later item before the drop target', () => {
    expect(reorderById(items, 'c', 'a').map((item) => item.id)).toEqual(['c', 'a', 'b']);
  });

  it('keeps the queue unchanged for unknown ids', () => {
    expect(reorderById(items, 'missing', 'a')).toEqual(items);
  });
});
