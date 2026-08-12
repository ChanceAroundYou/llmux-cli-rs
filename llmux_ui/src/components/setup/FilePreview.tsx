// 快速配置：单文件预览卡（行级 diff 或纯文本，右上角复制）
import React from "react";
import { CopyButton } from "@/components/shared/CopyButton";
import { DiffViewer } from "@/components/shared/DiffViewer";

interface FileCardProps {
  title: string;
  currentContent: string | null;
  previewContent: string | null;
  isDiff: boolean;
  /** 语法高亮语言（如 "json"、"toml"、"ini"）——传给 DiffViewer / 纯展示 */
  language?: string;
}

export const FileCard: React.FC<FileCardProps> = ({
  title,
  currentContent,
  previewContent,
  isDiff,
  language,
}) => {
  const showDiff = isDiff && currentContent != null && previewContent != null;
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
        {showDiff ? (
          <DiffViewer
            oldValue={currentContent}
            newValue={previewContent}
            maxHeight="320px"
            highlightLanguage={language}
          />
        ) : (
          // 纯展示：走库的高亮渲染（旧值同新值 → 全 unchanged，非新增），保留语法色
          <DiffViewer
            oldValue={displayContent ?? ""}
            newValue={displayContent ?? ""}
            maxHeight="320px"
            highlightLanguage={language}
            showDiffOnly={false}
          />
        )}
      </div>
    </div>
  );
};
