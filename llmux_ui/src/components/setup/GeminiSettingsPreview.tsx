// 快速配置：Gemini 配置预览（.env + settings.json 行级 diff）
import React, { useEffect, useState } from "react";
import { FileJson, RotateCcw } from "lucide-react";
import { useT } from "@/i18n";
import { cn } from "@/utils/helpers";
import { FileCard } from "./FilePreview";

interface Props {
  currentEnv: string | null;
  previewEnv: string | null;
  currentSettings: string | null;
  previewSettings: string | null;
  exists: boolean;
  loading: boolean;
  onRefresh: () => void;
}

export const GeminiSettingsPreview: React.FC<Props> = ({
  currentEnv,
  previewEnv,
  currentSettings,
  previewSettings,
  exists,
  loading,
  onRefresh,
}) => {
  const { t } = useT();
  const [tab, setTab] = useState<"diff" | "current">("diff");
  const hasPreview = !!(previewEnv || previewSettings);

  useEffect(() => {
    setTab(hasPreview ? "diff" : "current");
  }, [hasPreview]);

  const isDiff = tab === "diff" && hasPreview;
  const hasCurrent = exists && (!!currentEnv || !!currentSettings);

  return (
    <div className="space-y-2">
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-1 rounded-lg bg-muted/40 p-0.5">
          {hasPreview && (
            <button
              onClick={() => setTab("diff")}
              className={cn(
                "rounded-md px-2.5 py-1 text-xs font-semibold transition-all",
                tab === "diff"
                  ? "bg-card text-foreground shadow-sm"
                  : "text-muted-foreground hover:text-foreground",
              )}
            >
              <FileJson size={11} className="mr-1 inline" />
              {t("setup.tab.diff")}
            </button>
          )}
          <button
            onClick={() => setTab("current")}
            className={cn(
              "rounded-md px-2.5 py-1 text-xs font-semibold transition-all",
              tab === "current"
                ? "bg-card text-foreground shadow-sm"
                : "text-muted-foreground hover:text-foreground",
            )}
          >
            {hasCurrent ? t("setup.tab.current") : t("setup.tab.preview")}
          </button>
        </div>
        <div className="flex items-center gap-2">
          {exists && (
            <span className="font-mono text-xs text-muted-foreground">
              ~/.gemini/
            </span>
          )}
          <button
            onClick={onRefresh}
            className="rounded-lg p-1 transition-colors hover:bg-muted"
            title={t("setup.refresh")}
          >
            <RotateCcw size={11} className="text-muted-foreground" />
          </button>
        </div>
      </div>

      {loading ? (
        <div className="text-xs italic text-muted-foreground">
          {t("setup.loading")}
        </div>
      ) : (
        <div className="space-y-2">
          <FileCard
            title=".env"
            currentContent={currentEnv}
            previewContent={previewEnv}
            isDiff={isDiff}
            emptyText={t("setup.emptyFile")}
          />
          <FileCard
            title="settings.json"
            currentContent={currentSettings}
            previewContent={previewSettings}
            isDiff={isDiff}
            emptyText={t("setup.emptyFile")}
          />
        </div>
      )}
    </div>
  );
};
