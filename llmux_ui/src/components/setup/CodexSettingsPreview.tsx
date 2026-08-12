// 快速配置：Codex 配置预览（auth.json + config.toml 行级 diff）
import React, { useEffect, useState } from "react";
import { FileJson, RotateCcw } from "lucide-react";
import { useT } from "@/i18n";
import { cn } from "@/utils/helpers";
import { FileCard } from "./FilePreview";

interface Props {
  currentAuth: Record<string, unknown> | null;
  previewAuth: Record<string, unknown> | null;
  currentToml: string | null;
  previewToml: string | null;
  exists: boolean;
  loading: boolean;
  onRefresh: () => void;
}

export const CodexSettingsPreview: React.FC<Props> = ({
  currentAuth,
  previewAuth,
  currentToml,
  previewToml,
  exists,
  loading,
  onRefresh,
}) => {
  const { t } = useT();
  const [tab, setTab] = useState<"diff" | "current">("diff");
  const hasPreview = !!(previewAuth || previewToml);

  useEffect(() => {
    setTab(hasPreview ? "diff" : "current");
  }, [hasPreview]);

  const isDiff = tab === "diff" && hasPreview;
  const hasCurrent = exists && (!!currentAuth || !!currentToml);

  const authCurrent = currentAuth ? JSON.stringify(currentAuth, null, 2) : null;
  const authPreview = previewAuth ? JSON.stringify(previewAuth, null, 2) : null;
  const tomlCurrent = currentToml && String(currentToml).trim() ? currentToml : null;
  const tomlPreview = previewToml && String(previewToml).trim() ? previewToml : null;

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
              ~/.codex/
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
            title="auth.json"
            currentContent={authCurrent}
            previewContent={authPreview}
            isDiff={isDiff}
            language="json"
          />
          <FileCard
            title="config.toml"
            currentContent={tomlCurrent}
            previewContent={tomlPreview}
            isDiff={isDiff}
            language="toml"
          />
        </div>
      )}
    </div>
  );
};
