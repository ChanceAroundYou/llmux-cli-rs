import React from 'react';
import { ChevronRight } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { cn } from './utils';
import { TOOLS } from './types';

interface Props {
  selectedTool: string;
  installed: Record<string, boolean>;
  detectLoaded: boolean;
  onSelect: (id: string) => void;
}

export function ToolSidebar({ selectedTool, installed, detectLoaded, onSelect }: Props) {
  const { t } = useTranslation();
  return (
    <div className="w-56 shrink-0 border-r border-border pr-4 space-y-1 pt-1">
      <div className="text-[10px] font-bold text-muted-foreground uppercase tracking-widest px-2 pb-2">
        {t('setup.tools')}
      </div>
      {TOOLS.map(tt => {
        const Icon = tt.icon;
        const active = selectedTool === tt.id;
        const detected = installed[tt.detectKey] === true;
        return (
          <button
            key={tt.id}
            onClick={() => onSelect(tt.id)}
            className={cn(
              'w-full flex items-center gap-3 px-3 py-2.5 rounded-xl text-left transition-all',
              active ? 'bg-primary/10 text-primary' : 'text-muted-foreground hover:bg-muted/50 hover:text-foreground',
              !detected && detectLoaded && 'opacity-50'
            )}
          >
            <Icon size={15} className="shrink-0" />
            <div className="min-w-0 flex-1">
              <div className="flex items-center gap-1.5">
                <span className="text-xs font-semibold truncate">{tt.label}</span>
                {detectLoaded && (
                  <span
                    className={cn('w-1.5 h-1.5 rounded-full shrink-0', detected ? 'bg-green-500' : 'bg-muted-foreground/30')}
                    title={detected ? t('setup.installed') : t('setup.notInstalled')}
                  />
                )}
              </div>
              <div className="text-[10px] text-muted-foreground truncate">{tt.description}</div>
            </div>
            {active && <ChevronRight size={12} className="ml-auto shrink-0" />}
          </button>
        );
      })}
    </div>
  );
}
