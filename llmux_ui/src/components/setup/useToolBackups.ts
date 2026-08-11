// 快速配置：工具配置备份的通用状态机（列表/展开/还原/删除）
import { useCallback, useEffect, useState } from "react";
import { systemApi } from "@/api/system";
import { useToast } from "@/hooks/useToast";
import { useT } from "@/i18n";
import type { BackupEntry } from "@/types/system";

export type BackupTool = "claude" | "codex" | "gemini";

interface ToolBackups {
  backups: BackupEntry[];
  backupsLoading: boolean;
  isRestoring: boolean;
  expandedBackup: string | null;
  backupContents: Record<string, Record<string, unknown>>;
  dirtyModalOpen: boolean;
  pendingRestoreName: string | null;
  deleteModalName: string | null;
  fetchBackups: () => Promise<void>;
  toggleExpand: (name: string) => Promise<void>;
  /** 还原：表单有未应用修改时先弹确认，否则直接回填 */
  handleRestoreClick: (
    name: string,
    isDirty: boolean,
    fill: (content: Record<string, unknown>) => void,
  ) => Promise<void>;
  confirmRestore: (fill: (content: Record<string, unknown>) => void) => void;
  /** 确认删除当前 deleteModalName 对应备份 */
  handleDeleteConfirm: () => Promise<void>;
  setDeleteModalName: (name: string | null) => void;
  setDirtyModalOpen: (open: boolean) => void;
  setPendingRestoreName: (name: string | null) => void;
}

export function useToolBackups(tool: BackupTool): ToolBackups {
  const toast = useToast();
  const { t } = useT();
  const [backups, setBackups] = useState<BackupEntry[]>([]);
  const [backupsLoading, setBackupsLoading] = useState(false);
  const [expandedBackup, setExpandedBackup] = useState<string | null>(null);
  const [backupContents, setBackupContents] = useState<
    Record<string, Record<string, unknown>>
  >({});
  const [isRestoring, setIsRestoring] = useState(false);
  const [dirtyModalOpen, setDirtyModalOpen] = useState(false);
  const [pendingRestoreName, setPendingRestoreName] = useState<string | null>(
    null,
  );
  const [deleteModalName, setDeleteModalName] = useState<string | null>(null);

  const fetchBackups = useCallback(async () => {
    setBackupsLoading(true);
    try {
      setBackups(await systemApi.getBackups(tool));
    } catch {
      toast.error(t("setup.backupLoadFailed"));
    } finally {
      setBackupsLoading(false);
    }
  }, [tool, toast, t]);

  useEffect(() => {
    void fetchBackups();
  }, [fetchBackups]);

  // 读取备份详情并缓存（还原/展开共用）
  const loadDetail = useCallback(
    async (name: string) => {
      let content = backupContents[name];
      if (!content) {
        const data = await systemApi.getBackups(tool, name);
        if (!data?.settings) return null;
        content = data.settings as Record<string, unknown>;
        setBackupContents((prev) => ({ ...prev, [name]: content! }));
      }
      return content ?? null;
    },
    [backupContents, tool],
  );

  const toggleExpand = useCallback(
    async (name: string) => {
      if (expandedBackup === name) {
        setExpandedBackup(null);
        return;
      }
      setExpandedBackup(name);
      try {
        await loadDetail(name);
      } catch {
        toast.error(t("setup.backupLoadFailed"));
      }
    },
    [expandedBackup, loadDetail, toast, t],
  );

  const handleRestoreClick = useCallback(
    async (
      name: string,
      isDirty: boolean,
      fill: (content: Record<string, unknown>) => void,
    ) => {
      if (isRestoring) return;
      setIsRestoring(true);
      try {
        const content = await loadDetail(name);
        if (!content) return;
        if (isDirty) {
          setPendingRestoreName(name);
          setDirtyModalOpen(true);
        } else {
          fill(content);
        }
      } catch {
        toast.error(t("setup.backupLoadFailed"));
      } finally {
        setIsRestoring(false);
      }
    },
    [isRestoring, loadDetail, toast, t],
  );

  const confirmRestore = useCallback(
    (fill: (content: Record<string, unknown>) => void) => {
      setDirtyModalOpen(false);
      if (pendingRestoreName) {
        const content = backupContents[pendingRestoreName];
        if (content) fill(content);
      }
      setPendingRestoreName(null);
    },
    [pendingRestoreName, backupContents],
  );

  const handleDeleteConfirm = useCallback(async () => {
    if (!deleteModalName) return;
    try {
      await systemApi.deleteBackup(tool, deleteModalName);
      if (expandedBackup === deleteModalName) setExpandedBackup(null);
      setDeleteModalName(null);
      void fetchBackups();
    } catch {
      toast.error(t("setup.backupDeleteFailed"));
    }
  }, [deleteModalName, expandedBackup, tool, fetchBackups, toast, t]);

  return {
    backups,
    backupsLoading,
    isRestoring,
    expandedBackup,
    backupContents,
    dirtyModalOpen,
    pendingRestoreName,
    deleteModalName,
    fetchBackups,
    toggleExpand,
    handleRestoreClick,
    confirmRestore,
    handleDeleteConfirm,
    setDeleteModalName,
    setDirtyModalOpen,
    setPendingRestoreName,
  };
}
