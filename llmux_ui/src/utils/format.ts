// 格式化工具：时间相对化文案走 i18n
import { useI18n } from "@/i18n";

export function formatTimestamp(unixSeconds: number): string {
  const date = new Date(unixSeconds * 1000);
  const now = new Date();
  const diff = now.getTime() - date.getTime();
  const { t, lang } = useI18n.getState();

  if (diff < 60_000) return t("time.justNow");
  if (diff < 3_600_000) return t("time.minutesAgo", { n: Math.floor(diff / 60_000) });
  if (diff < 86_400_000) return t("time.hoursAgo", { n: Math.floor(diff / 3_600_000) });

  // 超过一天走绝对时间，locale 跟随当前语言
  const locale = lang === "zh" ? "zh-CN" : "en-US";
  return date.toLocaleString(locale, {
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}

export function formatLatency(ms: number): string {
  if (ms < 1000) return `${ms}ms`;
  return `${(ms / 1000).toFixed(2)}s`;
}

export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1_048_576) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / 1_048_576).toFixed(1)} MB`;
}
