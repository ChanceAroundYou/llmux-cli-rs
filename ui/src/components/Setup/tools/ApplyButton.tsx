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
}

export function ApplyButton({ selectedKey, applying, settingsExists, applyResult, onApply }: Props) {
  const { t } = useTranslation();
  return (
    <div className="space-y-2">
      <button
        onClick={onApply}
        disabled={!selectedKey || applying}
        className={cn(
          'w-full flex items-center justify-center gap-2 py-2.5 rounded-xl text-sm font-bold transition-all',
          selectedKey && !applying
            ? 'bg-primary text-primary-foreground hover:opacity-90'
            : 'bg-muted text-muted-foreground cursor-not-allowed',
        )}
      >
        {applying ? (
          <><RotateCcw size={14} className="animate-spin" />{t('setup.applying')}</>
        ) : (
          <><Zap size={14} />{settingsExists ? t('setup.applyBtn') : t('setup.initBtn')}</>
        )}
      </button>

      {applyResult && (
        <div className={cn(
          'p-3 rounded-xl text-xs space-y-1',
          applyResult.success
            ? 'bg-green-500/10 border border-green-500/20 text-green-600 dark:text-green-400'
            : 'bg-red-500/10 border border-red-500/20 text-red-500',
        )}>
          {applyResult.success ? (
            <>
              <div className="flex items-center gap-1.5 font-bold"><Check size={12} />{t('setup.applySuccess')}</div>
              {applyResult.backupPath && (
                <div className="text-muted-foreground font-mono break-all text-[10px]">
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
