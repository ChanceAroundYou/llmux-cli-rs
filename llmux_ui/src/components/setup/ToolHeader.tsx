// 快速配置：工具头部（名称 + 已安装徽标 + 未安装引导安装）
import React from "react";
import { AlertCircle, ExternalLink } from "lucide-react";
import { useT } from "@/i18n";
import { cn } from "@/utils/helpers";
import type { ToolDef } from "./types";

interface Props {
  tool: ToolDef;
  isInstalled: boolean;
  detectLoaded: boolean;
}

export const ToolHeader: React.FC<Props> = ({
  tool,
  isInstalled,
  detectLoaded,
}) => {
  const { t } = useT();
  const Icon = tool.icon;
  return (
    <div className="space-y-3">
      <div className="flex items-center gap-3 border-b border-border pb-2">
        <div className="rounded-xl bg-primary/10 p-2 text-primary">
          <Icon size={18} />
        </div>
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2">
            <h2 className="text-base font-semibold">{t(tool.labelKey)}</h2>
            {detectLoaded && (
              <span
                className={cn(
                  "inline-flex items-center gap-1 rounded px-1.5 py-0.5 text-xs font-semibold",
                  isInstalled
                    ? "bg-success/10 text-success"
                    : "bg-muted text-muted-foreground",
                )}
              >
                <span
                  className={cn(
                    "h-1 w-1 rounded-full",
                    isInstalled ? "bg-success" : "bg-muted-foreground/40",
                  )}
                />
                {isInstalled ? t("setup.installed") : t("setup.notInstalled")}
              </span>
            )}
          </div>
          <p className="text-xs text-muted-foreground">{t(tool.descKey)}</p>
        </div>
      </div>

      {detectLoaded && !isInstalled && !tool.comingSoon && (
        <div className="flex items-center gap-3 rounded-xl border border-warning/20 bg-warning/5 p-4 text-sm">
          <AlertCircle size={16} className="shrink-0 text-warning" />
          <span className="flex-1 text-foreground">
            {t("setup.notInstalledHint", { tool: t(tool.labelKey) })}
          </span>
          <a
            href={tool.installUrl}
            target="_blank"
            rel="noopener noreferrer"
            className="flex items-center gap-1.5 rounded-lg bg-primary px-3 py-1.5 text-xs font-semibold text-primary-foreground transition-opacity hover:opacity-90"
          >
            {t("setup.install")}
            <ExternalLink size={11} />
          </a>
        </div>
      )}
    </div>
  );
};
