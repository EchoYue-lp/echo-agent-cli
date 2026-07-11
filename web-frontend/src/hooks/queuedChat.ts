export function reorderById<T extends { id: string }>(
  items: readonly T[],
  sourceId: string,
  targetId: string
): T[] {
  if (sourceId === targetId) return [...items];
  const next = [...items];
  const sourceIndex = next.findIndex((item) => item.id === sourceId);
  const targetIndex = next.findIndex((item) => item.id === targetId);
  if (sourceIndex < 0 || targetIndex < 0) return next;
  const [source] = next.splice(sourceIndex, 1);
  if (!source) return next;
  next.splice(targetIndex, 0, source);
  return next;
}
