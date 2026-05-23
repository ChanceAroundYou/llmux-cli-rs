/**
 * 将服务器返回的时间戳转换为本地 Date 对象
 * 支持：毫秒数字（新版）、SQLite UTC 字符串（旧版兼容）
 */
export function parseServerDate(ts: string | number): Date {
  if (!ts) return new Date();
  // 毫秒数字直接转
  if (typeof ts === 'number') return new Date(ts);
  // 如果已经是标准 ISO 格式 (带 T)，直接解析
  if (ts.includes('T')) return new Date(ts);
  // 针对 SQLite 默认格式补全 T 和 Z（旧版兼容）
  const iso = ts.replace(' ', 'T') + 'Z';
  const date = new Date(iso);
  return isNaN(date.getTime()) ? new Date(ts) : date;
}

/**
 * 格式化服务器时间为本地可读字符串
 */
export function formatServerDate(ts: string | number, options?: Intl.DateTimeFormatOptions): string {
  return parseServerDate(ts).toLocaleString(undefined, options);
}

/**
 * 格式化服务器时间为本地时间字符串
 */
export function formatServerTime(ts: string | number, options?: Intl.DateTimeFormatOptions): string {
  return parseServerDate(ts).toLocaleTimeString(undefined, options);
}
