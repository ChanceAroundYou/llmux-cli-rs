import React from 'react';
import { useTranslation } from 'react-i18next';
import { History, RotateCcw, ChevronDown, ChevronUp, ArchiveRestore, Trash2 } from 'lucide-react';
import { cn } from '../utils';
import { CopyButton } from '../../CopyButton';
import { ConfirmDialog } from '../../Modal';

export interface BackupEntry {
  name: string;
  path: string;
  timestamp: string;
  size: number;
}

interface Props {
  backups: BackupEntry[];
  backupsLoading: boolean;
  isRestoring: boolean;
  pendingRestoreName: string | null;
  expandedBackup: string | null;
  backupContents: Record<string, Record<string, any>>;
  dirtyModalOpen: boolean;
  deleteModalName: string | null;
  onToggleExpand: (name: string) => void;
  onRestoreClick: (name: string) => void;
  onDeleteClick: (name: string) => void;
  onDirtyModalClose: () => void;
  onDirtyModalConfirm: () => void;
  onDeleteModalClose: () => void;
  onDeleteConfirm: () => void;
}

export function BackupHistory({
  backups,
  backupsLoading,
  isRestoring,
  pendingRestoreName,
  expandedBackup,
  backupContents,
  dirtyModalOpen,
  deleteModalName,
  onToggleExpand,
  onRestoreClick,
  onDeleteClick,
  onDirtyModalClose,
  onDirtyModalConfirm,
  onDeleteModalClose,
  onDeleteConfirm,
}: Props) {
  const { t } = useTranslation();

  return (
    <>
      <div className="border border-border rounded-xl overflow-hidden">
        <div className="flex items-center gap-2 px-4 py-3 border-b border-border bg-muted/20">
          <History size={14} className="text-muted-foreground shrink-0" />
          <span className="text-xs font-bold flex-1">{t('setup.backupHistory')}</span>
          {backupsLoading && <RotateCcw size={11} className="animate-spin text-muted-foreground" />}
          <span className="text-[10px] text-muted-foreground">{t('setup.backupMax')}</span>
        </div>

        <div className="max-h-[480px] overflow-y-auto divide-y divide-border">
          {backups.length === 0 ? (
            <div className="px-4 py-4 text-xs text-muted-foreground">{t('setup.noBackups')}</div>
          ) : (
            backups.map(b => {
              const isExpanded = expandedBackup === b.name;
              const content = backupContents[b.name];
              return (
                <div key={b.name}>
                  <div className="flex items-center gap-2 px-4 py-2.5 hover:bg-muted/30 transition-colors">
                    <button
                      onClick={() => onToggleExpand(b.name)}
                      className="flex-1 flex items-center gap-2 text-left min-w-0"
                    >
                      {isExpanded
                        ? <ChevronUp size={12} className="text-muted-foreground shrink-0" />
                        : <ChevronDown size={12} className="text-muted-foreground shrink-0" />
                      }
                      <div className="min-w-0">
                        <div className="text-xs font-mono text-foreground/80">{b.timestamp}</div>
                        <div className="text-[10px] text-muted-foreground">{(b.size / 1024).toFixed(1)} KB</div>
                      </div>
                    </button>

                    <button
                      onClick={() => onRestoreClick(b.name)}
                      disabled={isRestoring}
                      className={cn(
                        'flex items-center gap-1 px-2 py-1 rounded-lg border border-border text-[11px] font-semibold transition-colors shrink-0',
                        isRestoring ? 'opacity-40 cursor-not-allowed' : 'hover:bg-muted/50',
                      )}
                    >
                      {isRestoring && pendingRestoreName === b.name
                        ? <RotateCcw size={11} className="animate-spin" />
                        : <ArchiveRestore size={11} />
                      }
                      {t('setup.restore')}
                    </button>

                    <button
                      onClick={() => onDeleteClick(b.name)}
                      className="p-1.5 rounded-lg text-muted-foreground hover:text-red-500 hover:bg-red-500/10 transition-colors shrink-0"
                      title={t('setup.deleteBackup')}
                    >
                      <Trash2 size={13} />
                    </button>
                  </div>

                  {isExpanded && (
                    <div className="border-t border-border bg-muted/10">
                      {!content ? (
                        <div className="px-4 py-3 text-xs text-muted-foreground">{t('setup.loading')}</div>
                      ) : (
                        <div className="relative">
                          <div className="absolute top-2 right-2 z-10">
                            <CopyButton value={JSON.stringify(content, null, 2)} size={12} />
                          </div>
                          <pre className="px-4 py-3 text-[11px] font-mono text-foreground/80 whitespace-pre overflow-x-auto">
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
        isOpen={dirtyModalOpen}
        onClose={onDirtyModalClose}
        onConfirm={onDirtyModalConfirm}
        title={t('setup.dirtyConfirmTitle')}
        description={t('setup.dirtyConfirm')}
        confirmText={t('setup.discardAndRestore')}
        variant="warning"
      />

      <ConfirmDialog
        isOpen={!!deleteModalName}
        onClose={onDeleteModalClose}
        onConfirm={onDeleteConfirm}
        title={t('setup.deleteBackupTitle')}
        description={t('setup.deleteBackupConfirm', {
          name: deleteModalName?.replace('settings.json.', '') ?? '',
        })}
        confirmText={t('setup.delete')}
        variant="danger"
      />
    </>
  );
}
