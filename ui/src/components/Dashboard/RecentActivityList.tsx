import React, { useState } from 'react';
import { History, AlertTriangle } from 'lucide-react';
import { parseServerDate } from '../../utils/date';
import { TFunction } from 'i18next';
import { cn } from '../../lib/utils';

interface ActivityEntry {
  id: number;
  timestamp: number;
  model: string;
  success: number;
  latency_ms: number;
  error_message: string | null;
  account_name: string;
  provider_id: string;
}

interface ActivityItemProps {
  model: string;
  time: string;
  status: 'success' | 'error';
  latency: string;
  accountName?: string;
  errorMessage?: string | null;
}

const ActivityItem = ({ model, time, status, latency, accountName, errorMessage }: ActivityItemProps) => (
  <div className="flex items-center justify-between py-2.5 border-b border-border/40 last:border-0 hover:bg-muted/50 px-2 rounded-lg transition-colors group">
    <div className="flex items-center gap-3 min-w-0">
      <div className={cn(
        "w-1.5 h-1.5 rounded-full shrink-0",
        status === 'success' ? "bg-success shadow-[0_0_8px_rgba(34,197,94,0.4)]" : "bg-destructive shadow-[0_0_8px_rgba(239,68,68,0.4)]"
      )} />
      <div className="min-w-0">
        <div className="text-xs font-semibold truncate leading-tight">{model}</div>
        <div className="flex items-center gap-1.5 mt-0.5">
          <span className="text-[9px] text-muted-foreground/60 font-medium">{time}</span>
          {accountName && (
            <>
              <span className="text-[8px] opacity-20">|</span>
              <span className="text-[9px] text-primary/50 font-semibold">{accountName}</span>
            </>
          )}
        </div>
        {status === 'error' && errorMessage && (
          <div className="text-[9px] text-destructive/80 truncate mt-0.5 max-w-[200px]" title={errorMessage}>
            {errorMessage}
          </div>
        )}
      </div>
    </div>
    <div className="text-xs font-mono font-semibold text-muted-foreground/40 group-hover:text-muted-foreground transition-colors shrink-0">{latency}</div>
  </div>
);

interface RecentActivityListProps {
  recentLogs: ActivityEntry[];
  t: TFunction;
}

export const RecentActivityList = ({ recentLogs, t }: RecentActivityListProps) => {
  const [showErrorsOnly, setShowErrorsOnly] = useState(false);

  const filteredLogs = showErrorsOnly ? recentLogs.filter(l => l.success !== 1) : recentLogs;

  const successCount = recentLogs.filter(l => l.success === 1).length;
  const errorCount = recentLogs.length - successCount;
  const successRate = recentLogs.length ? Math.round((successCount / recentLogs.length) * 100) : 0;
  const avgLatency = recentLogs.length
    ? Math.round(recentLogs.reduce((acc, l) => acc + (l.latency_ms || 0), 0) / recentLogs.length)
    : 0;

  return (
    <div className="premium-card flex flex-col h-full bg-card border-border/60">
      <div className="flex items-center justify-between mb-6">
        <div className="flex items-center gap-2">
          <History size={16} className="text-primary" />
          <span className="text-sm font-semibold text-foreground/80">{t('dashboard.recentLogs')}</span>
        </div>
        <div className="flex items-center gap-3">
          <div className="flex items-center gap-3 text-xs text-muted-foreground">
            <span>{successRate}% {t('dashboard.monitor.successRate')}</span>
            <span>{(avgLatency / 1000).toFixed(1)}s {t('dashboard.monitor.avgLag')}</span>
          </div>
          <button
            onClick={() => setShowErrorsOnly(!showErrorsOnly)}
            className={cn(
              "flex items-center gap-1 px-2 py-1 rounded text-xs font-semibold transition-all",
              showErrorsOnly
                ? "bg-destructive/10 text-destructive"
                : "bg-muted/50 text-muted-foreground hover:text-foreground"
            )}
          >
            <AlertTriangle size={10} />
            {showErrorsOnly ? t('dashboard.showAll') : `${t('dashboard.errorsOnly')}${errorCount > 0 ? ` (${errorCount})` : ''}`}
          </button>
        </div>
      </div>

      <div className="flex-1 min-h-0">
        <div className="space-y-1 overflow-y-auto max-h-[500px] pr-2">
          {filteredLogs.length > 0 ? filteredLogs.slice(0, 20).map((log) => (
            <ActivityItem
              key={log.id}
              model={log.model}
              time={parseServerDate(log.timestamp).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', hour12: false })}
              status={log.success === 1 ? 'success' : 'error'}
              latency={`${((log.latency_ms || 0) / 1000).toFixed(1)}s`}
              accountName={log.account_name}
              errorMessage={log.error_message}
            />
          )) : (
            <div className="py-20 text-center text-muted-foreground/20 text-xs font-semibold uppercase border border-dashed border-border/60 rounded-xl">
              {showErrorsOnly ? t('dashboard.noErrors') : t('dashboard.noActivity')}
            </div>
          )}
        </div>
      </div>
    </div>
  );
};
