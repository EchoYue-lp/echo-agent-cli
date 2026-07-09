/** SVG 圆环周长（r=10 → 2πr）。 */
export const CONTEXT_RING_CIRCUMFERENCE = 2 * Math.PI * 10;

/** 按占用比例计算 stroke-dashoffset（0=满环，circumference=空环）。 */
export function ringDashOffset(ratio: number | null): number {
  if (ratio == null || ratio <= 0) return CONTEXT_RING_CIRCUMFERENCE;
  const clamped = Math.min(1, Math.max(0, ratio));
  return CONTEXT_RING_CIRCUMFERENCE * (1 - clamped);
}
