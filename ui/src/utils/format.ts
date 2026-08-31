export const fmtSec = (ms: number | null | undefined): string =>
  typeof ms === 'number' && Number.isFinite(ms) ? `${(ms / 1000).toFixed(1)}s` : '—';

// Header carries the unit (耗时(s) / TTFT(s) / Token/s); rows show bare numbers.
export const fmtSecNum = (ms: number | null | undefined): string =>
  typeof ms === 'number' && Number.isFinite(ms) ? (ms / 1000).toFixed(1) : '—';

export const fmtTpsNum = (n: number | null | undefined): string =>
  typeof n === 'number' && Number.isFinite(n) ? n.toFixed(1) : '—';

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

// 中间省略：>20 字符时极短版，头取前两段、尾只留末段（muse-spark-1.2-contributor-free → muse-spark…free）
export const abbrModel = (s: string): string => {
  if (!s || s.length <= 20) return s;
  const parts = s.split('-');
  if (parts.length >= 3) return `${parts[0]}-${parts[1]}…${parts[parts.length - 1]}`;
  return `${s.slice(0, 10)}…${s.slice(-4)}`;
};
