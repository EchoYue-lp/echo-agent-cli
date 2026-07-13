import { describe, expect, it } from 'vitest';
import { imagePoint } from './BrowserViewport';

describe('imagePoint', () => {
  it('maps displayed screenshot coordinates to browser viewport coordinates', () => {
    expect(imagePoint(210, 120, { left: 10, top: 20, width: 400, height: 200 }, 1200, 600)).toEqual(
      { x: 600, y: 300 }
    );
  });

  it('ignores clicks outside the rendered screenshot', () => {
    expect(imagePoint(-10, 50, { left: 0, top: 0, width: 200, height: 100 }, 800, 400)).toBeNull();
  });

  it('accounts for object-contain letterboxing', () => {
    expect(imagePoint(10, 50, { left: 0, top: 0, width: 300, height: 100 }, 800, 400)).toBeNull();
    expect(imagePoint(150, 50, { left: 0, top: 0, width: 300, height: 100 }, 800, 400)).toEqual({
      x: 400,
      y: 200,
    });
  });

  it('rejects an image without measurable dimensions', () => {
    expect(imagePoint(0, 0, { left: 0, top: 0, width: 0, height: 100 }, 800, 400)).toBeNull();
  });
});
