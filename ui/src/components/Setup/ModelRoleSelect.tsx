import React from 'react';
import { ChevronDown } from 'lucide-react';
import { cn } from './utils';

interface Props {
  label: string;
  envKey: string;
  models: string[];
  value: string;
  longContext: boolean;
  onChange: (v: string) => void;
  onLongContextChange: (v: boolean) => void;
}

export function ModelRoleSelect({ label, envKey, models, value, longContext, onChange, onLongContextChange }: Props) {
  const effectiveValue = value ? (longContext ? `${value}[1m]` : value) : '';

  return (
    <div className="space-y-1.5">
      <div className="flex items-center justify-between">
        <span className="text-[11px] font-bold text-muted-foreground uppercase tracking-wide">{label}</span>
        <span className="text-[10px] text-muted-foreground/60 font-mono">{envKey}</span>
      </div>
      <div className="flex gap-2">
        <div className="relative flex-1">
          <select
            value={value}
            onChange={e => onChange(e.target.value)}
            className="w-full appearance-none bg-card border border-border rounded-lg px-3 py-2 text-xs font-medium pr-7 focus:outline-none focus:border-primary transition-colors"
          >
            <option value="">— 不设置 —</option>
            {models.map(m => (
              <option key={m} value={m}>{m}</option>
            ))}
          </select>
          <ChevronDown size={12} className="absolute right-2.5 top-1/2 -translate-y-1/2 text-muted-foreground pointer-events-none" />
        </div>
        {/* 1m 开关 */}
        <button
          type="button"
          disabled={!value}
          onClick={() => onLongContextChange(!longContext)}
          title="启用百万上下文 [1m]"
          className={cn(
            'shrink-0 px-2 rounded-lg border text-[10px] font-bold transition-all',
            !value && 'opacity-30 cursor-not-allowed',
            value && longContext
              ? 'bg-primary/15 border-primary/40 text-primary'
              : 'bg-card border-border text-muted-foreground hover:border-muted-foreground/50'
          )}
        >
          1m
        </button>
      </div>
      {/* 实际写入值预览 */}
      {effectiveValue && (
        <div className="text-[10px] font-mono text-muted-foreground/70 pl-1">
          → <span className="text-foreground/80">{effectiveValue}</span>
        </div>
      )}
    </div>
  );
}
