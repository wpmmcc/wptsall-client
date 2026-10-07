/** Preserve zero, default invalid input, and honor the form's numeric bounds. */
export function configInteger(
  value: string | number | undefined,
  fallback: number,
  min: number,
  max = Number.MAX_SAFE_INTEGER,
): number {
  const parsed = Number.parseInt(String(value ?? ''), 10);
  return Math.max(min, Math.min(max, Number.isFinite(parsed) ? parsed : fallback));
}
