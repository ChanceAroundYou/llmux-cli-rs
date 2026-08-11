// 快速配置：左侧工具列表（含本机安装检测状态点）
import React from "react";
import { ChevronRight } from "lucide-react";
import { useT } from "@/i18n";
import { cn } from "@/utils/helpers";
import { TOOLS } from "./types";

interface Props {
  selectedTool: string;
  installed: Record<string, boolean>;
  detectLoaded: boolean;
  onSelect: (id: string) => void;
}

export const ToolSidebar: React.FC<Props> = ({
  selectedTool,
  installed,
  detectLoaded,
  onSelect,
}) => {
  const { t } = useT();
  return (
    <div className="shrink-0 border-b border-border pb-2 xl:w-56 xl:border-b-0 xl:border-r xl:pb-0 xl:pr-4 xl:pt-1">
      <div className="hidden px-2 pb-2 text-xs font-semibold uppercase tracking-widest text-muted-foreground xl:block">
        {t("setup.tools")}
      </div>
      {/* 移动端横向滚动，xl 纵向排列 */}
      <div className="flex gap-1.5 overflow-x-auto pb-1 xl:flex-col xl:gap-0 xl:space-y-1 xl:overflow-visible xl:pb-0">
        {TOOLS.map((tool) => {
          const Icon = tool.icon;
          const active = selectedTool === tool.id;
          const detected = installed[tool.detectKey] === true;
          return (
            <button
              key={tool.id}
              onClick={() => onSelect(tool.id)}
              className={cn(
                "flex shrink-0 items-center gap-3 rounded-xl px-3 py-2.5 text-left transition-all xl:w-full",
                active
                  ? "bg-primary/10 text-primary"
                  : "text-muted-foreground hover:bg-muted/50 hover:text-foreground",
                detectLoaded && !detected && "opacity-50",
              )}
            >
              <Icon size={15} className="shrink-0" />
              <div className="min-w-0 flex-1">
                <div className="flex items-center gap-1.5">
                  <span className="truncate text-xs font-semibold">
                    {t(tool.labelKey)}
                  </span>
                  {detectLoaded && (
                    <span
                      className={cn(
                        "h-1.5 w-1.5 shrink-0 rounded-full",
                        detected ? "bg-success" : "bg-muted-foreground/30",
                      )}
                      title={
                        detected
                          ? t("setup.installed")
                          : t("setup.notInstalled")
                      }
                    />
                  )}
                </div>
                <div className="truncate text-xs text-muted-foreground xl:block hidden">
                  {t(tool.descKey)}
                </div>
              </div>
              {active && (
                <ChevronRight size={12} className="ml-auto hidden shrink-0 xl:block" />
              )}
            </button>
          );
        })}
      </div>
    </div>
  );
};
