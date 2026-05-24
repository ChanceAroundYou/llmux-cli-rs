import React from 'react';
import { useTranslation } from 'react-i18next';
import { Zap, RotateCcw, Check, AlertCircle } from 'lucide-react';
import { cn } from '../utils';

interface Props {
  selectedKey: boolean;
  applying: boolean;
  settingsExists: boolean;
  applyResult: { success: boolean; backupPath?: string; error?: string } | null;
  onApply: () => void;
  applyLabel?: string;
  initLabel?: string;
}

export function ApplyButton({ selectedKey, applying, settingsExists, applyResult, onApply, applyLabel, initLabel }: Props) {
  const { t } = useTranslation();
  return (
    <div className="space-y-2">
      <button
        onClick={onApply}
        disabled={!selectedKey || applying}
        className={cn(
          'w-full flex items-center justify-center gap-2 py-2.5 rounded-xl text-sm font-semibold transition-all',
          selectedKey && !applying
            ? 'bg-primary text-primary-foreground hover:opacity-90'
            : 'bg-muted text-muted-foreground cursor-not-allowed',
        )}
      >
        {applying ? (
          <><RotateCcw size={14} className="animate-spin" />{t('setup.applying')}</>
        ) : (
          <><Zap size={14} />{settingsExists ? (applyLabel ?? t('setup.applyBtn')) : (initLabel ?? t('setup.initBtn'))}</>
        )}
      </button>

      {applyResult && (
        <div className={cn(
          'p-3 rounded-xl text-xs space-y-1',
          applyResult.success
            ? 'bg-success/10 border border-success/20 text-success dark:text-success'
            : 'bg-destructive/10 border border-destructive/20 text-destructive',
        )}>
          {applyResult.success ? (
            <>
              <div className="flex items-center gap-1.5 font-semibold"><Check size={12} />{t('setup.applySuccess')}</div>
              {applyResult.backupPath && (
                <div className="text-muted-foreground font-mono break-all text-xs">
                  {t('setup.backupAt')}{applyResult.backupPath}
                </div>
              )}
            </>
          ) : (
            <div className="flex items-center gap-1.5"><AlertCircle size={12} />{applyResult.error}</div>
          )}
        </div>
      )}
    </div>
  );
}
