// 真实分布校准 (usage_logs 成功请求, 2026-08-30, n≈20k):
//   TTFT  stream: P50 7.9s  P75 15.3s  P80 18.4s  P85 22.4s
//   net   stream (lat-ttft): P50 1.4s  P75 4.1s  P85 7.6s  P90 12.0s
//   total all:    P50 10.6s P75 20.4s  P85 29.3s
//   alias_health(n=33): P50 7.5s P75 15.0s P85 23.7s
// 旧阈值 0.8s/1.5s (TTFT) / 2s (lat) / 0.5s/1.2s (健康) → P5 就超阈，>95% 飘红。
// 新阈值取整对齐 P50≈绿/黄分界、P80-85≈黄/红分界，50/30/20 分布可读。

export const TTFT_WARN = 8_000;
export const TTFT_BAD = 18_000;

export const NET_WARN = 5_000;
export const NET_BAD = 12_000;

export const TOTAL_WARN = 10_000;
export const TOTAL_BAD = 25_000;

// 别名健康探测与 TTFT 同源
export const HEALTH_WARN = TTFT_WARN;
export const HEALTH_BAD = TTFT_BAD;

export type Tone = 'ok' | 'warn' | 'bad';

const toneFor = (v: number | null | undefined, warn: number, bad: number): Tone | null => {
  if (typeof v !== 'number' || !Number.isFinite(v)) return null;
  if (v > bad) return 'bad';
  if (v > warn) return 'warn';
  return 'ok';
};

export const ttftTone = (v: number | null | undefined): Tone | null => toneFor(v, TTFT_WARN, TTFT_BAD);
export const netTone = (v: number | null | undefined): Tone | null => toneFor(v, NET_WARN, NET_BAD);
export const totalTone = (v: number | null | undefined): Tone | null => toneFor(v, TOTAL_WARN, TOTAL_BAD);
export const healthTone = (v: number | null | undefined): Tone | null => toneFor(v, HEALTH_WARN, HEALTH_BAD);

// Tailwind 文本色：复用 logs TTFT 列的语义（ok=前景色，避免整表飘绿）
export const ttftTextClass = (v: number | null | undefined): string => {
  const t = ttftTone(v);
  if (t === 'bad') return 'text-destructive';
  if (t === 'warn') return 'text-warning';
  return 'text-foreground';
};

// 脉冲图色板：与 Tailwind 语义对齐
const GREEN = '#22c55e';
const AMBER = '#f59e0b';
const RED = '#ef4444';

export const chartColorForTtft = (ttft: number | null | undefined, success: number | boolean): string => {
  if (success !== 1 && success !== true) return RED;
  const t = ttftTone(typeof ttft === 'number' ? ttft : null);
  if (t === 'bad') return RED;
  if (t === 'warn') return AMBER;
  return GREEN;
};

export const chartColorForLatency = (lat: number | null | undefined, success: number | boolean): string => {
  if (success !== 1 && success !== true) return RED;
  const t = totalTone(typeof lat === 'number' ? lat : null);
  if (t === 'bad') return RED;
  if (t === 'warn') return AMBER;
  return GREEN;
};

export const chartBarHeight = (tone: Tone | null, success: number | boolean): number => {
  if (success !== 1 && success !== true) return 0.8;
  if (tone === 'bad') return 1.2;
  if (tone === 'warn') return 1.15;
  return 1;
};

// 别名健康徽章：ok 绿 / warn 黄 / bad 红（原 >1.2s 用蓝 primary，已按真实分布校正为红）
export const healthBadgeClass = (latency: number | null, success: boolean): string => {
  if (!success) return 'bg-destructive/10 text-destructive border-destructive/20';
  const t = healthTone(latency);
  if (t === 'bad') return 'bg-destructive/10 text-destructive border-destructive/20';
  if (t === 'warn') return 'bg-warning/10 text-warning border-warning/20';
  return 'bg-success/10 text-success border-success/20';
};
