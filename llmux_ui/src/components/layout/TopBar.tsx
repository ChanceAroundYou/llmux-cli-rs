// 全局顶栏：网关连接状态 + 主题/语言切换 + 移动端菜单
import React, { useEffect, useState } from "react";
import { cn } from "@/utils/helpers";
import { useT } from "@/i18n";
import { useThemeStore, getSystemTheme, applyThemeClass, type Scheme } from "@/stores/theme";
import { systemApi } from "@/api/system";
import { CopyButton } from "@/components/shared/CopyButton";
import { Sun, Moon, Monitor, Menu } from "lucide-react";

const themeOptions: { value: Scheme; icon: typeof Sun; labelKey: string }[] = [
  { value: "system", icon: Monitor, labelKey: "theme.system" },
  { value: "light", icon: Sun, labelKey: "theme.light" },
  { value: "dark", icon: Moon, labelKey: "theme.dark" },
];

interface TopBarProps {
  onMenuClick: () => void;
}

export const TopBar: React.FC<TopBarProps> = ({ onMenuClick }) => {
  const { t, lang, setLang } = useT();
  const scheme = useThemeStore((s) => s.scheme);
  const setScheme = useThemeStore((s) => s.setScheme);
  const palette = useThemeStore((s) => s.palette);

  // 后端连接状态：探测 /system/tools，在线显示呼吸绿点 + 网关地址，每 30s 刷新
  const [online, setOnline] = useState<boolean | null>(null);
  useEffect(() => {
    let mounted = true;
    const ping = async () => {
      try {
        await systemApi.getTools();
        if (mounted) setOnline(true);
      } catch {
        if (mounted) setOnline(false);
      }
    };
    void ping();
    const id = window.setInterval(ping, 30000);
    return () => {
      mounted = false;
      window.clearInterval(id);
    };
  }, []);
  const origin = window.location.origin;

  // 应用主题（色板 + 明暗）+ system 时监听系统偏好变化
  useEffect(() => {
    const apply = () => {
      const resolved = scheme === "system" ? getSystemTheme() : scheme;
      applyThemeClass(resolved, palette);
    };
    apply();
    if (scheme === "system") {
      const mq = window.matchMedia("(prefers-color-scheme: dark)");
      const handler = () => apply();
      mq.addEventListener("change", handler);
      return () => mq.removeEventListener("change", handler);
    }
  }, [scheme, palette]);

  return (
    <header className="sticky top-0 z-30 border-b border-border/60 bg-background/70 backdrop-blur-xl">
      <div className="flex h-14 items-center justify-between px-4 lg:px-8">
        {/* 左侧：移动端菜单 + 网关状态 */}
        <div className="flex min-w-0 items-center gap-3">
          <button
            onClick={onMenuClick}
            className="rounded-lg p-2 text-muted-foreground hover:bg-muted lg:hidden"
            aria-label="menu"
          >
            <Menu className="h-5 w-5" />
          </button>
          <div className="hidden items-center gap-2.5 sm:flex">
            {/* 连接状态：呼吸绿点 / 离线红点 / 探测中灰点 */}
            <span className="relative flex h-2 w-2 shrink-0">
              {online && (
                <span className="absolute inline-flex h-full w-full animate-ping rounded-full bg-success opacity-60" />
              )}
              <span
                className={cn(
                  "relative inline-flex h-2 w-2 rounded-full",
                  online === null
                    ? "bg-muted-foreground/40"
                    : online
                      ? "bg-success"
                      : "bg-destructive",
                )}
              />
            </span>
            <span className="text-xs font-medium text-muted-foreground">
              {online === null
                ? "…"
                : online
                  ? t("common.online")
                  : t("common.offline")}
            </span>
            {online && (
              <span className="flex items-center gap-1 rounded-lg border border-border bg-muted/40 py-0.5 pl-2">
                <span className="font-mono text-xs text-muted-foreground">
                  {origin}
                </span>
                <CopyButton text={origin} className="p-1" />
              </span>
            )}
          </div>
        </div>

        {/* 右侧：主题 + 语言切换 */}
        <div className="flex items-center gap-2">
          <div className="flex items-center gap-0.5 rounded-full bg-muted p-0.5">
            {themeOptions.map((opt) => {
              const Icon = opt.icon;
              const active = scheme === opt.value;
              return (
                <button
                  key={opt.value}
                  title={t(opt.labelKey)}
                  onClick={() => setScheme(opt.value)}
                  className={cn(
                    "flex h-7 w-7 items-center justify-center rounded-full transition-all duration-200",
                    active
                      ? "bg-card text-primary shadow-soft"
                      : "text-muted-foreground hover:text-foreground",
                  )}
                >
                  <Icon className="h-4 w-4" />
                </button>
              );
            })}
          </div>

          <div className="flex items-center gap-0.5 rounded-full bg-muted p-0.5">
            {(["zh", "en"] as const).map((l) => (
              <button
                key={l}
                onClick={() => setLang(l)}
                className={cn(
                  "rounded-full px-2.5 py-1 text-xs font-semibold transition-all duration-200",
                  lang === l
                    ? "bg-card text-primary shadow-soft"
                    : "text-muted-foreground hover:text-foreground",
                )}
              >
                {l === "zh" ? "中" : "EN"}
              </button>
            ))}
          </div>
        </div>
      </div>
    </header>
  );
};
