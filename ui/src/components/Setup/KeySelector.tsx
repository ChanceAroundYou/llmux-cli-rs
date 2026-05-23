import React from 'react';
import { Key, ChevronRight, AlertCircle } from 'lucide-react';
import { Link } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import { cn } from './utils';
import type { ApiKey } from '../../stores/keys';

interface Props {
  keys: ApiKey[];
  selectedKeyId: number | '';
  onSelect: (id: number) => void;
}

export function KeySelector({ keys, selectedKeyId, onSelect }: Props) {
  const { t } = useTranslation();
  if (keys.length === 0) {
    return (
      <div className="flex items-start gap-3 p-4 rounded-xl border border-dashed border-border bg-muted/30 text-sm text-muted-foreground">
        <AlertCircle size={15} className="mt-0.5 shrink-0 text-amber-500" />
        <span>
          {t('setup.noKeys')}{' '}
          <Link to="/keys" className="text-primary underline underline-offset-2 font-medium">
            {t('setup.createKey')}
          </Link>
        </span>
      </div>
    );
  }
  return (
    <div className="space-y-1.5">
      {keys.map(k => (
        <button
          key={k.id}
          onClick={() => onSelect(k.id)}
          className={cn(
            'w-full flex items-center gap-3 px-3 py-2.5 rounded-xl border text-left transition-all',
            selectedKeyId === k.id ? 'border-primary bg-primary/5' : 'border-border bg-card hover:bg-muted/50'
          )}
        >
          <Key size={13} className={selectedKeyId === k.id ? 'text-primary' : 'text-muted-foreground'} />
          <div className="flex-1 min-w-0">
            <div className="text-xs font-semibold truncate">{k.name}</div>
            <div className="text-[10px] text-muted-foreground font-mono truncate">
              {k.key.slice(0, 12)}••••••••
            </div>
          </div>
          {selectedKeyId === k.id && <ChevronRight size={12} className="text-primary shrink-0" />}
        </button>
      ))}
    </div>
  );
}
