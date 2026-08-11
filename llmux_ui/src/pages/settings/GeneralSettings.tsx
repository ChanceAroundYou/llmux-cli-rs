// 系统设置：外观（多色板主题）+ 危险区域（清空数据）
import React, { useEffect, useState } from "react";
import { settingsApi } from "@/api/settings";
import { Card } from "@/components/ui/Card";
import { Button } from "@/components/ui/Button";
import { ConfirmDialog } from "@/components/ui/ConfirmDialog";
import { useToast } from "@/hooks/useToast";
import { useT } from "@/i18n";
import { cn } from "@/utils/helpers";
import { useThemeStore, PALETTES, getSystemTheme, applyThemeClass, type Scheme } from "@/stores/theme";
import { Check, Monitor, Moon, Sun } from "lucide-react";

const schemeOptions: { value: Scheme; icon: typeof Sun; labelKey: string }[] = [
  { value: "system", icon: Monitor, labelKey: "theme.system" },
  { value: "light", icon: Sun, labelKey: "theme.light" },
  { value: "dark", icon: Moon, labelKey: "theme.dark" },
];

export const GeneralSettings: React.FC = () => {
  const { t } = useT();
  const scheme = useThemeStore((s) => s.scheme);
  const palette = useThemeStore((s) => s.palette);
  const setScheme = useThemeStore((s) => s.setScheme);
  const setPalette = useThemeStore((s) => s.setPalette);

  // 设置页内切换主题立即生效（与 TopBar 同步）
  useEffect(() => {
    const resolved = scheme === "system" ? getSystemTheme() : scheme;
    applyThemeClass(resolved, palette);
  }, [scheme, palette]);

  const [resetOpen, setResetOpen] = useState(false);
  const [resetting, setResetting] = useState(false);
  const toast = useToast();

  const handleReset = async () => {
    setResetting(true);
    try {
      await settingsApi.reset();
      toast.success(t("settings.reset.success"));
      setResetOpen(false);
    } catch {
      toast.error(t("settings.reset.failed"));
    } finally {
      setResetting(false);
    }
  };

  return (
    <div className="animate-fade-in space-y-6">
      {/* 外观：多色板 + 明暗 */}
      <Card title={t("settings.appearance.title")} description={t("settings.appearance.desc")}>
        <div className="space-y-6">
          <div>
            <p className="mb-2.5 text-sm font-medium text-card-foreground">
              {t("theme.palette.label")}
            </p>
            <div className="flex flex-wrap gap-4">
              {PALETTES.map((p) => (
                <button
                  key={p.id}
                  type="button"
                  onClick={() => setPalette(p.id)}
                  className="group flex flex-col items-center gap-1.5"
                >
                  <span
                    className={cn(
                      "flex h-10 w-10 items-center justify-center rounded-full border-2 transition-all group-hover:scale-105",
                      palette === p.id
                        ? "scale-105 border-primary shadow-soft"
                        : "border-border/60",
                    )}
                    style={{ background: p.color }}
                  >
                    {palette === p.id && <Check className="h-4 w-4 text-white" />}
                  </span>
                  <span className="text-xs text-muted-foreground">
                    {t(p.labelKey)}
                  </span>
                </button>
              ))}
            </div>
          </div>

          <div>
            <p className="mb-2.5 text-sm font-medium text-card-foreground">
              {t("theme.label")}
            </p>
            <div className="inline-flex items-center gap-0.5 rounded-full bg-muted p-0.5">
              {schemeOptions.map((opt) => {
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
          </div>
        </div>
      </Card>

      <Card title={t("settings.danger.title")} className="border-destructive/30">
        <div className="flex flex-wrap items-center justify-between gap-3">
          <div>
            <p className="text-sm font-medium text-card-foreground">{t("settings.reset.title")}</p>
            <p className="mt-0.5 text-xs text-muted-foreground">{t("settings.reset.desc")}</p>
          </div>
          <Button variant="danger" onClick={() => setResetOpen(true)}>
            {t("settings.reset.button")}
          </Button>
        </div>
      </Card>

      <ConfirmDialog
        open={resetOpen}
        title={t("settings.reset.title")}
        message={t("settings.reset.confirmMsg")}
        danger
        confirmText={t("settings.reset.confirmText")}
        loading={resetting}
        onConfirm={handleReset}
        onCancel={() => setResetOpen(false)}
      />
    </div>
  );
};
