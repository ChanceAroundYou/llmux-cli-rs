import React from 'react';
import { Activity } from 'lucide-react';
import { TFunction } from 'i18next';

const cn = (...classes: (string | boolean | undefined | null)[]) => classes.filter(Boolean).join(' ');

export interface ProviderHealth {
  id: string;
  name?: string;
  status: 'healthy' | 'degraded' | 'down' | 'unknown';
  totalChecks: number;
}

interface HealthStatusListProps {
  healthStatus: ProviderHealth[];
  t: TFunction;
}

export const HealthStatusList = ({ healthStatus, t }: HealthStatusListProps) => {
  return (
    <div className="premium-card bg-gradient-to-br from-card to-primary/[0.02] flex flex-col">
      <div className="flex items-center gap-2 text-xs font-bold text-muted-foreground uppercase tracking-widest mb-6">
        <Activity size={14} className="text-primary" />
        {t('dashboard.providerStatus')}
      </div>
      <div className="space-y-3 overflow-y-auto max-h-[220px] no-scrollbar">
        {healthStatus.map((v) => (
          <div key={v.id} className="flex items-center justify-between p-3 bg-muted/40 rounded-xl border border-border/50 group hover:border-primary/20 transition-all">
            <div className="flex items-center gap-3">
              <div className="w-8 h-8 rounded-lg bg-background flex items-center justify-center font-bold text-[10px] border border-border group-hover:scale-110 transition-transform">
                {(v.name || v.id).slice(0, 2).toUpperCase()}
              </div>
              <div>
                <div className="text-sm font-bold capitalize">{v.name || v.id}</div>
                <div className="flex items-center gap-1.5 mt-0.5">
                  <div className={cn(
                    "w-1.5 h-1.5 rounded-full",
                    v.status === 'healthy' && "bg-green-500 shadow-[0_0_8px_rgba(34,197,94,0.4)]",
                    v.status === 'degraded' && "bg-amber-500",
                    v.status === 'down' && "bg-red-500",
                    v.status === 'unknown' && "bg-muted-foreground/30"
                  )} />
                  <span className="text-[10px] text-muted-foreground font-medium">
                    {v.status === 'unknown' ? t('dashboard.noTraffic') : t(`common.${v.status}`)}
                  </span>
                </div>
              </div>
            </div>
            <div className="text-right shrink-0">
              <div className="text-sm font-bold tabular-nums">{v.totalChecks}</div>
              <div className="text-[9px] uppercase text-muted-foreground">{t('dashboard.calls')}</div>
            </div>
          </div>
        ))}
      </div>
    </div>
  );
};
