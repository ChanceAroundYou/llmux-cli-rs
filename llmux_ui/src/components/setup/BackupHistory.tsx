// 快速配置：备份历史（展开查看 / 还原 / 删除，含 dirty 与删除确认弹窗）
import React from "react";
import {
  ArchiveRestore,
  ChevronDown,
  ChevronUp,
  History,
  RotateCcw,
  Trash2,
} from "lucide-react";
import { useT } from "@/i18n";
import { cn } from "@/utils/helpers";
import { CopyButton } from "@/components/shared/CopyButton";
import { ConfirmDialog } from "@/components/ui/ConfirmDialog";
import type { BackupEntry } from "@/types/system";

interface Props {
  backups: BackupEntry[];
  backupsLoading: boolean;
  isRestoring: boolean;
  expandedBackup: string | null;
  backupContents: Record<string, Record<string, unknown>>;
  dirtyModalOpen: boolean;
  pendingRestoreName: string | null;
  deleteModalName: string | null;
  onToggleExpand: (name: string) => void;
  onRestoreClick: (name: string) => void;
  onDeleteClick: (name: string) => void;
  onDirtyModalClose: () => void;
  onDirtyModalConfirm: () => void;
  onDeleteModalClose: () => void;
  onDeleteConfirm: () => void;
}

export const BackupHistory: React.FC<Props> = ({
  backups,
  backupsLoading,
  isRestoring,
  expandedBackup,
  backupContents,
  dirtyModalOpen,
  pendingRestoreName,
  deleteModalName,
  onToggleExpand,
  onRestoreClick,
  onDeleteClick,
  onDirtyModalClose,
  onDirtyModalConfirm,
  onDeleteModalClose,
  onDeleteConfirm,
}) => {
  const { t } = useT();

  return (
    <>
      <div className="overflow-hidden rounded-xl border border-border">
        <div className="flex items-center gap-2 border-b border-border bg-muted/20 px-4 py-3">
          <History size={14} className="shrink-0 text-muted-foreground" />
          <span className="flex-1 text-xs font-semibold">
            {t("setup.backupHistory")}
          </span>
          {backupsLoading && (
            <RotateCcw
              size={11}
              className="animate-[spin_1s_linear_infinite_reverse] text-muted-foreground"
            />
          )}
          <span className="text-xs text-muted-foreground">
            {t("setup.backupMax")}
          </span>
        </div>

        <div className="max-h-[480px] divide-y divide-border overflow-y-auto">
          {backups.length === 0 ? (
            <div className="px-4 py-4 text-xs text-muted-foreground">
              {t("setup.noBackups")}
            </div>
          ) : (
            backups.map((b) => {
              const isExpanded = expandedBackup === b.name;
              const content = backupContents[b.name];
              return (
                <div key={b.name}>
                  <div className="flex items-center gap-2 px-4 py-2.5 transition-colors hover:bg-muted/30">
                    <button
                      onClick={() => onToggleExpand(b.name)}
                      className="flex min-w-0 flex-1 items-center gap-2 text-left"
                    >
                      {isExpanded ? (
                        <ChevronUp size={12} className="shrink-0 text-muted-foreground" />
                      ) : (
                        <ChevronDown size={12} className="shrink-0 text-muted-foreground" />
                      )}
                      <div className="min-w-0">
                        <div className="truncate font-mono text-xs text-foreground/80">
                          {b.timestamp}
                        </div>
                        <div className="text-xs text-muted-foreground">
                          {(b.size / 1024).toFixed(1)} KB
                        </div>
                      </div>
                    </button>

                    <button
                      onClick={() => onRestoreClick(b.name)}
                      disabled={isRestoring}
                      className={cn(
                        "flex shrink-0 items-center gap-1 rounded-lg border border-border px-2 py-1 text-xs font-semibold transition-colors",
                        isRestoring
                          ? "cursor-not-allowed opacity-40"
                          : "hover:bg-muted/50",
                      )}
                    >
                      {isRestoring && pendingRestoreName === b.name ? (
                        <RotateCcw
                          size={11}
                          className="animate-[spin_1s_linear_infinite_reverse]"
                        />
                      ) : (
                        <ArchiveRestore size={11} />
                      )}
                      {t("setup.restore")}
                    </button>

                    <button
                      onClick={() => onDeleteClick(b.name)}
                      className="shrink-0 rounded-lg p-1.5 text-muted-foreground transition-colors hover:bg-destructive/10 hover:text-destructive"
                      title={t("setup.deleteBackup")}
                    >
                      <Trash2 size={13} />
                    </button>
                  </div>

                  {isExpanded && (
                    <div className="border-t border-border bg-muted/10">
                      {!content ? (
                        <div className="px-4 py-3 text-xs text-muted-foreground">
                          {t("setup.loading")}
                        </div>
                      ) : (
                        <div className="relative">
                          <div className="absolute right-2 top-2 z-10">
                            <CopyButton
                              text={JSON.stringify(content, null, 2)}
                            />
                          </div>
                          <pre className="overflow-x-auto whitespace-pre px-4 py-3 font-mono text-xs text-foreground/80">
                            {JSON.stringify(content, null, 2)}
                          </pre>
                        </div>
                      )}
                    </div>
                  )}
                </div>
              );
            })
          )}
        </div>
      </div>

      <ConfirmDialog
        open={dirtyModalOpen}
        title={t("setup.dirtyConfirmTitle")}
        message={t("setup.dirtyConfirm")}
        confirmText={t("setup.discardAndRestore")}
        danger
        onConfirm={onDirtyModalConfirm}
        onCancel={onDirtyModalClose}
      />

      <ConfirmDialog
        open={!!deleteModalName}
        title={t("setup.deleteBackupTitle")}
        message={t("setup.deleteBackupConfirm", {
          name: deleteModalName?.replace(/^(?:settings\.json|codex)\./, "") ?? "",
        })}
        confirmText={t("setup.delete")}
        danger
        onConfirm={onDeleteConfirm}
        onCancel={onDeleteModalClose}
      />
    </>
  );
};
