import React from 'react';
import { AlertCircle, ExternalLink } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { cn } from './utils';
import { ToolDef } from './types';

interface Props {
  tool: ToolDef;
  isInstalled: boolean;
  detectLoaded: boolean;
}

export function ToolHeader({ tool, isInstalled, detectLoaded }: Props) {
  const { t } = useTranslation();
  const Icon = tool.icon;
  return (
    <div className="space-y-3">
      <div className="flex items-center gap-3 pb-2 border-b border-border">
        <div className="p-2 bg-primary/10 text-primary rounded-xl">
          <Icon size={18} />
        </div>
        <div className="flex-1 min-w-0">
          <div className="flex items-center gap-2">
            <h2 className="text-base font-bold">{tool.label}</h2>
            {detectLoaded && (
              <span className={cn(
                'inline-flex items-center gap-1 px-1.5 py-0.5 rounded text-[10px] font-bold',
                isInstalled
                  ? 'bg-green-500/10 text-green-600 dark:text-green-400'
                  : 'bg-muted text-muted-foreground'
              )}>
                <span className={cn('w-1 h-1 rounded-full', isInstalled ? 'bg-green-500' : 'bg-muted-foreground/40')} />
                {isInstalled ? t('setup.installed') : t('setup.notInstalled')}
              </span>
            )}
          </div>
          <p className="text-xs text-muted-foreground">{tool.description}</p>
        </div>
      </div>

      {detectLoaded && !isInstalled && (
        <div className="flex items-center gap-3 p-4 rounded-xl border border-amber-500/20 bg-amber-500/5 text-sm">
          <AlertCircle size={16} className="shrink-0 text-amber-500" />
          <span className="flex-1 text-foreground">{t('setup.notInstalledHint', { tool: tool.label })}</span>
          <a
            href={tool.installUrl}
            target="_blank"
            rel="noopener noreferrer"
            className="flex items-center gap-1.5 px-3 py-1.5 rounded-lg bg-primary text-primary-foreground text-xs font-semibold hover:opacity-90 transition-opacity"
          >
            {t('setup.install')}<ExternalLink size={11} />
          </a>
        </div>
      )}
    </div>
  );
}
