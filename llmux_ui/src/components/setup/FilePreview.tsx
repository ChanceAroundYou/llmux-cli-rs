// 快速配置：单文件预览卡（行级 diff 或纯文本，右上角复制）
import React, { useMemo } from "react";
import { useT } from "@/i18n";
import { cn } from "@/utils/helpers";
import { computeLineDiff, type DiffLine } from "./utils";
import { CopyButton } from "@/components/shared/CopyButton";

export const DiffLines: React.FC<{ lines: DiffLine[] }> = ({ lines }) => {
  const { t } = useT();
  const hasChanges = lines.some((l) => l.type !== "unchanged");
  return (
    <div className="space-y-px overflow-x-auto font-mono text-xs leading-relaxed">
      {!hasChanges && (
        <div className="mb-1 italic text-muted-foreground/50">
          {t("setup.noChanges")}
        </div>
      )}
      {lines.map((l, i) => (
        <div
          key={i}
          className={cn(
            "flex items-start gap-2 whitespace-nowrap rounded px-2",
            l.type === "removed" && "bg-destructive/10 text-destructive",
            l.type === "added" && "bg-success/10 text-success",
            l.type === "unchanged" && "text-muted-foreground/80",
          )}
        >
          <span className="w-3 shrink-0 select-none">
            {l.type === "removed" ? "−" : l.type === "added" ? "+" : " "}
          </span>
          <span>{l.line}</span>
        </div>
      ))}
    </div>
  );
};

interface FileCardProps {
  title: string;
  currentContent: string | null;
  previewContent: string | null;
  isDiff: boolean;
  emptyText?: string;
}

export const FileCard: React.FC<FileCardProps> = ({
  title,
  currentContent,
  previewContent,
  isDiff,
  emptyText = "— —",
}) => {
  const diffLines = useMemo(() => {
    if (!isDiff || !currentContent || !previewContent) return null;
    return computeLineDiff(currentContent, previewContent);
  }, [isDiff, currentContent, previewContent]);

  const displayContent = isDiff && previewContent ? previewContent : currentContent;

  return (
    <div className="overflow-hidden rounded-xl border border-border bg-muted/20">
      <div className="flex items-center justify-between border-b border-border/40 bg-muted/10 px-4 py-2">
        <div className="flex items-center gap-2">
          <div className="flex gap-1">
            <div className="h-2 w-2 rounded-full bg-destructive/60" />
            <div className="h-2 w-2 rounded-full bg-warning/60" />
            <div className="h-2 w-2 rounded-full bg-success/60" />
          </div>
          <span className="text-[10px] font-bold uppercase tracking-widest text-muted-foreground">
            {title}
          </span>
        </div>
        <CopyButton text={displayContent ?? ""} />
      </div>
      <div className="max-h-[360px] overflow-y-auto p-3">
        {diffLines ? (
          <DiffLines lines={diffLines} />
        ) : (
          <pre className="whitespace-pre overflow-x-auto font-mono text-[10px] leading-relaxed text-foreground/70">
            {displayContent ?? emptyText}
          </pre>
        )}
      </div>
    </div>
  );
};
