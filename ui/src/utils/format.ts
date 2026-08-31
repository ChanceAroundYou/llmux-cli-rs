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

// 智能中间省略（>20 才触发）：按 '-' 词边界保留，尽量多保留头段信息，避免把单词切成 tor-free
//  muse-spark-1.2-contributor-free → muse-spark-1.2…free (19) 优于 muse-spark…free (16) 更完整，且不切词
export const abbrModel = (s: string): string => {
  if (!s || s.length <= 20) return s;
  // 含分隔符的按词边界缩写（'-' '/' '_' 均视为边界）
  const m = s.match(/^(.+)([-/_])([^-/_]+)$/);
  if (!m) return `${s.slice(0, 10)}…${s.slice(-8)}`;
  const tail = m[3]; // 末段，如 free
  const tailWithSep = `${m[2]}${tail}`; // -free
  // 头部按 '-' 切段，贪心多保留直到总长触及 20（含 …）
  const headRaw = s.slice(0, s.length - tailWithSep.length); // 去掉尾段，保留 head 含分隔符
  const parts = headRaw.split(/[-/_]/).filter(Boolean);
  // 重建 head：逐步加段直到超预算回退一步
  let best = parts.slice(0, 2).join('-');
  if (!best) best = headRaw.slice(0, 10).replace(/[-/_]$/, '');
  for (let n = 3; n <= parts.length; n++) {
    const cand = parts.slice(0, n).join('-');
    const total = `${cand}…${tail}`.length;
    if (total <= 20) best = cand;
    else break;
  }
  // 兜底：若 best 仍使总长超 20（极长段），截断到 12
  const abbr = `${best}…${tail}`;
  if (abbr.length <= 22) return abbr;
  return `${best.slice(0, 12)}…${tail}`;
};
