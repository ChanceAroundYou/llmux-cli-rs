// 快速配置：Claude settings 预览（Diff/当前 双 tab，按扁平 key 对比高亮）
import React, { useEffect, useMemo, useState } from "react";
import { FileJson, RotateCcw } from "lucide-react";
import { useT } from "@/i18n";
import { cn } from "@/utils/helpers";
import { buildKeyDiff, type DiffLine } from "./utils";
import { CopyButton } from "@/components/shared/CopyButton";

// 高亮展示写入网关的关键字段
const HIGHLIGHT_KEYS = new Set([
  "ANTHROPIC_BASE_URL",
  "ANTHROPIC_AUTH_TOKEN",
  "ANTHROPIC_DEFAULT_OPUS_MODEL",
  "ANTHROPIC_DEFAULT_SONNET_MODEL",
  "ANTHROPIC_DEFAULT_HAIKU_MODEL",
]);

function DiffView({
  lines,
}: {
  lines: DiffLine[];
}) {
  const { t } = useT();
  const hasChanges = lines.some((l) => l.type !== "unchanged");
  return (
    <div className="space-y-px overflow-x-auto font-mono text-xs leading-relaxed">
      {!hasChanges && (
        <div className="mb-1 italic text-muted-foreground/50">
          {t("setup.noChanges")}
        </div>
      )}
      {lines.map((l, i) => {
        const leafKey = l.key?.split(".").pop() ?? "";
        const highlighted = HIGHLIGHT_KEYS.has(leafKey);
        return (
          <div
            key={i}
            className={cn(
              "flex items-start gap-2 whitespace-nowrap rounded px-2",
              l.type === "removed" && "bg-destructive/10 text-destructive",
              l.type === "added" && "bg-success/10 text-success",
              l.type === "unchanged" &&
                (highlighted ? "text-foreground/70" : "text-muted-foreground/80"),
            )}
          >
            <span className="w-3 shrink-0 select-none">
              {l.type === "removed" ? "−" : l.type === "added" ? "+" : " "}
            </span>
            <span className={cn(highlighted && l.type !== "unchanged" && "font-semibold")}>
              {l.line}
            </span>
          </div>
        );
      })}
    </div>
  );
}

interface Props {
  /** 当前文件内容（settings.json 对象） */
  settings: Record<string, unknown> | null;
  /** 将要写入的内容 */
  preview: Record<string, unknown> | null;
  exists: boolean;
  loading: boolean;
  onRefresh: () => void;
}

export const SettingsPreview: React.FC<Props> = ({
  settings,
  preview,
  exists,
  loading,
  onRefresh,
}) => {
  const { t } = useT();
  const [tab, setTab] = useState<"diff" | "current">("diff");

  // 有预览时默认 diff，无预览显示当前/预览内容
  useEffect(() => {
    setTab(preview ? "diff" : "current");
  }, [preview]);

  const diffLines = useMemo(() => {
    if (!preview) return null;
    return buildKeyDiff(settings, preview);
  }, [settings, preview]);

  const showContent = tab === "current" ? (settings ?? preview) : preview;

  return (
    <div className="space-y-2">
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-1 rounded-lg bg-muted/40 p-0.5">
          {preview && (
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
            {exists ? t("setup.tab.current") : t("setup.tab.preview")}
          </button>
        </div>
        <div className="flex items-center gap-2">
          {exists && (
            <span className="font-mono text-xs text-muted-foreground">
              ~/.claude/settings.json
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

      <div className="overflow-hidden rounded-xl border border-border bg-muted/20">
        <div className="flex items-center justify-between border-b border-border/40 bg-muted/10 px-4 py-2">
          <div className="flex gap-1.5">
            <div className="h-2 w-2 rounded-full bg-destructive/60" />
            <div className="h-2 w-2 rounded-full bg-warning/60" />
            <div className="h-2 w-2 rounded-full bg-success/60" />
          </div>
          <div className="flex items-center gap-1.5">
            {tab === "diff" && preview && (
              <span className="text-xs text-muted-foreground">
                {t("setup.changed")}
              </span>
            )}
            {showContent && (
              <CopyButton text={JSON.stringify(showContent, null, 2)} />
            )}
          </div>
        </div>
        <div className="max-h-[400px] overflow-y-auto p-4">
          {loading ? (
            <div className="font-mono text-xs text-muted-foreground">
              {t("setup.loading")}
            </div>
          ) : tab === "diff" && diffLines ? (
            <DiffView lines={diffLines} />
          ) : showContent ? (
            <pre className="whitespace-pre overflow-x-auto font-mono text-xs leading-relaxed text-foreground/80">
              {JSON.stringify(showContent, null, 2)}
            </pre>
          ) : (
            <div className="space-y-1 font-mono text-xs text-muted-foreground">
              <div>{t("setup.noSettingsFile")}</div>
              <div className="text-muted-foreground/50">
                # {t("setup.noSettingsHint")}
              </div>
            </div>
          )}
        </div>
      </div>
    </div>
  );
};
