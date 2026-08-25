export const fmtSec = (ms: number | null | undefined): string =>
  typeof ms === 'number' && Number.isFinite(ms) ? `${(ms / 1000).toFixed(1)}s` : '—';

export const netMs = (
  latencyMs: number,
  ttftMs: number | null | undefined,
  isStream: number | boolean | null | undefined,
): number => {
  const isS = typeof isStream === 'number' ? isStream === 1 : !!isStream;
  if (isS && typeof ttftMs === 'number' && ttftMs > 0 && ttftMs < latencyMs) return latencyMs - ttftMs;
  return latencyMs;
};

export const fmtTokens = (n: number | null | undefined): string => {
  const v = typeof n === 'number' && Number.isFinite(n) ? Math.round(n) : 0;
  if (v >= 1_000_000_000) return `${(v / 1_000_000_000).toFixed(1).replace(/\.0$/, '')}B`;
  if (v >= 1_000_000) return `${(v / 1_000_000).toFixed(1).replace(/\.0$/, '')}M`;
  if (v >= 1_000) return `${(v / 1_000).toFixed(1).replace(/\.0$/, '')}k`;
  return `${v}`;
};
