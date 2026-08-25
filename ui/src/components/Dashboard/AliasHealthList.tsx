import { Zap, ArrowRight } from 'lucide-react';
import { fmtSec } from '../../utils/format';
import { TFunction } from 'i18next';
import { cn } from '../../lib/utils';

export interface AliasHealth {
  alias: string;
  target_model: string;
  success: boolean;
  latency: number | null;
  error: string | null;
  lastChecked: number | null;
}

interface AliasHealthListProps {
  aliases: AliasHealth[];
  t: TFunction;
}

export const AliasHealthList = ({ aliases, t }: AliasHealthListProps) => {
  const healthy = aliases.filter(a => a.success).length;

  return (
    <div className="premium-card bg-gradient-to-br from-card to-primary/[0.02] flex flex-col">
      <div className="flex items-center gap-2 text-xs font-semibold text-muted-foreground uppercase tracking-widest mb-6">
        <Zap size={14} className="text-primary" />
        {t('dashboard.aliasHealth')}
        {aliases.length > 0 && (
          <span className="ml-auto text-primary font-normal normal-case tracking-normal">
            {healthy}/{aliases.length} {t('dashboard.healthy')}
          </span>
        )}
      </div>
      <div className="space-y-3 overflow-y-auto max-h-[320px] no-scrollbar">
        {aliases.length === 0 ? (
          <div className="py-16 text-center text-muted-foreground/30 text-xs font-semibold uppercase border border-dashed border-border/60 rounded-xl">
            {t('dashboard.noAliases')}
          </div>
        ) : (
          aliases.map((a, i) => (
            <div
              key={i}
              className="flex items-center justify-between p-3 bg-muted/40 rounded-xl border border-border/50 group hover:border-primary/20 transition-all"
            >
              <div className="flex items-center gap-3 min-w-0">
                <div
                  className={cn(
                    "w-2 h-2 rounded-full shrink-0",
                    a.success
                      ? "bg-success shadow-[0_0_8px_rgba(34,197,94,0.4)]"
                      : a.lastChecked
                        ? "bg-destructive shadow-[0_0_8px_rgba(239,68,68,0.4)]"
                        : "bg-muted-foreground/30"
                  )}
                />
                <div className="min-w-0">
                  <div className="text-sm font-semibold truncate">{a.alias}</div>
                  <div className="flex items-center gap-1 text-[9px] text-muted-foreground/60 mt-0.5">
                    <span className="truncate">{a.target_model}</span>
                  </div>
                </div>
              </div>
              <div className="text-right shrink-0 ml-2">
                {a.latency !== null ? (
                  <div className={cn("text-sm font-semibold tabular-nums", a.success ? "text-success" : "text-destructive")}>
                    {a.success ? fmtSec(a.latency) : 'ERR'}
                  </div>
                ) : a.lastChecked ? (
                  <div className="text-sm font-semibold text-destructive">ERR</div>
                ) : (
                  <div className="text-xs text-muted-foreground/40">{t('dashboard.untested')}</div>
                )}
              </div>
            </div>
          ))
        )}
      </div>
    </div>
  );
};
