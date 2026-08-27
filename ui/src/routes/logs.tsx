import React, { useCallback, useEffect, useState, useMemo } from 'react';
import { apiFetch } from '../lib/api';
import { useTranslation } from 'react-i18next';
import { useSearchParams } from 'react-router-dom';
import { Loader2, ScrollText, ChevronLeft, ChevronRight, Copy, Check } from 'lucide-react';
import { StatusDot } from '@/components/shared/StatusDot';
import { Dialog } from '../components/Modal';
import { JsonView } from '@/components/shared/JsonTree';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { Badge } from '@/components/ui/badge';
import { parseServerDate } from '../utils/date';
import { fmtSec, netMs } from '../utils/format';
import { cn } from '@/lib/utils';

// t/s：流式按生成段(总耗时-首字时间)，非流式按总耗时。
const calcTps = (outputTokens: number, latencyMs: number, ttftMs: number | null | undefined): number => {
  const total = latencyMs || 0;
  const ttft = typeof ttftMs === 'number' && ttftMs > 0 && ttftMs < total ? ttftMs : 0;
  const gen = Math.max(1, ttft ? total - ttft : total);
  return ((outputTokens || 0) * 1000) / gen;
};
const formatK = (n: number): string => {
  const v = Math.round(n || 0);
  if (v >= 1000) return `${(v / 1000).toFixed(1).replace(/\.0$/, '')}k`;
  return `${v}`;
};
const cacheOf = (l: LogEntry): number => {
  if (typeof (l as any).cache_tokens === 'number') return (l as any).cache_tokens;
  if (typeof (l as any).cacheTokens === 'number') return (l as any).cacheTokens;
  return (l.cacheReadInputTokens || 0); // 4-store-3-display: creation hidden
};

interface LogEntry {
  id: number;
  timestamp: number;
  accountId: number;
  providerId: string;
  model: string;
  inputTokens: number;
  outputTokens: number;
  cacheReadInputTokens: number;
  cacheCreationInputTokens: number;
  cache_tokens?: number;
  latencyMs: number;
  ttftMs: number | null;
  isStream: number;
  tps: number | null;
  success: boolean;
  errorMessage: string | null;
  isTest: number;
  accountName: string | null;
  clientIp?: string | null;
}

const PAGE_SIZE = 50;

type RangeKey = '24h' | '7d' | 'all';

export default function Logs() {
  const { t } = useTranslation();
  const [logs, setLogs] = useState<LogEntry[]>([]);
  const [isLoading, setIsLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [range, setRange] = useState<RangeKey>('24h');
  const [statusFilter, setStatusFilter] = useState<'all' | 'success' | 'failed'>('all');
  const [modelFilter, setModelFilter] = useState('');
  const [streamMode, setStreamMode] = useState<'all' | 'stream' | 'nonStream'>('all');
  const [page, setPage] = useState(0);
  const [total, setTotal] = useState(0);
  const [pageInput, setPageInput] = useState('');
  const [detailLog, setDetailLog] = useState<LogEntry | null>(null);
  const [detailData, setDetailData] = useState<{ request_body: string | null; response_body: string | null; client_ip?: string | null } | null>(null);
  const [detailLoading, setDetailLoading] = useState(false);
  const [searchParams, setSearchParams] = useSearchParams();
  const [blockOpen, setBlockOpen] = useState({ request: true, response: true });
  const [allExpanded, setAllExpanded] = useState<boolean | undefined>(undefined);
  const [treeKey, setTreeKey] = useState(0);
  const [copiedError, setCopiedError] = useState(false);

  const expandAllBlocks = () => {
    setAllExpanded(true);
    setBlockOpen({ request: true, response: true });
    setTreeKey(k => k + 1);
  };
  const collapseAllBlocks = () => {
    setAllExpanded(false);
    setBlockOpen({ request: false, response: false });
    setTreeKey(k => k + 1);
  };

  const openLogDetail = async (log: LogEntry) => {
    setDetailLog(log);
    setDetailData(null);
    setDetailLoading(true);
    try {
      const res = await apiFetch(`/api/activity/${log.id}`);
      if (res.ok) setDetailData(await res.json());
    } catch (err) { console.error('Failed to fetch log detail', err); }
    finally { setDetailLoading(false); }
  };

  const closeLogDetail = () => {
    setDetailLog(null);
    setSearchParams({}, { replace: true });
  };

  useEffect(() => {
    const logId = searchParams.get('log');
    if (!logId || detailLog) return;
    const target = logs.find(l => String(l.id) === logId);
    if (target) openLogDetail(target);
  }, [searchParams, logs, detailLog]);

  const totalPages = Math.max(1, Math.ceil(total / PAGE_SIZE));

  const jumpToPage = (p: number) => {
    const clamped = Math.min(Math.max(1, p), totalPages);
    setPage(clamped - 1);
    setPageInput('');
  };

  const fetchLogs = useCallback(async () => {
    setIsLoading(true);
    setError(null);
    try {
      const params = new URLSearchParams();
      params.set('limit', String(PAGE_SIZE));
      params.set('offset', String(page * PAGE_SIZE));
      if (range === '24h') params.set('start', String(Date.now() - 24 * 3600 * 1000));
      else if (range === '7d') params.set('start', String(Date.now() - 7 * 24 * 3600 * 1000));
      if (statusFilter !== 'all') params.set('success', statusFilter === 'success' ? '1' : '0');
      if (modelFilter.trim()) params.set('model', modelFilter.trim());
      const res = await apiFetch(`/api/stats/logs?${params.toString()}`);
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const data = await res.json();
      const entries: LogEntry[] = data.logs || [];
      setLogs(entries);
      setTotal(typeof data.total === 'number' ? data.total : entries.length);
    } catch (err: any) {
      setError(err.message || 'Failed to load logs');
    } finally {
      setIsLoading(false);
    }
  }, [range, statusFilter, modelFilter, page]);

  useEffect(() => {
    fetchLogs();
  }, [fetchLogs]);

  const resetPage = () => setPage(0);

  const displayLogs = useMemo(() => {
    return logs.filter(l => {
      if (streamMode === 'stream' && !l.isStream) return false;
      if (streamMode === 'nonStream' && l.isStream) return false;
      return true;
    });
  }, [logs, streamMode]);

  const copyError = async () => {
    if (!detailLog?.errorMessage) return;
    await navigator.clipboard.writeText(detailLog.errorMessage);
    setCopiedError(true);
    setTimeout(() => setCopiedError(false), 1500);
  };

  return (
    <div className="space-y-6 animate-fadeIn">
      <div className="flex items-start gap-3">
        <div className="p-2 bg-primary/10 text-primary rounded-lg mt-1.5">
          <ScrollText size={24} />
        </div>
        <div>
          <h1 className="text-2xl font-semibold tracking-tight">{t('logs.title')}</h1>
          <p className="text-sm text-muted-foreground">{t('logs.subtitle')}</p>
        </div>
      </div>

      <div className="flex flex-wrap items-center gap-3">
        <div className="flex border border-border rounded-lg overflow-hidden">
          {(['24h', '7d', 'all'] as RangeKey[]).map(k => (
            <Button
              key={k}
              variant={range === k ? "default" : "ghost"}
              size="sm"
              onClick={() => { setRange(k); resetPage(); }}
              className="rounded-none h-auto py-1.5 text-xs font-semibold"
            >
              {t(k === 'all' ? 'logs.allTime' : `logs.last${k}`)}
            </Button>
          ))}
        </div>
        <div className="flex border border-border rounded-lg overflow-hidden">
          {(['all', 'success', 'failed'] as const).map(k => (
            <Button
              key={k}
              variant={statusFilter === k ? "default" : "ghost"}
              size="sm"
              onClick={() => { setStatusFilter(k); resetPage(); }}
              className="rounded-none h-auto py-1.5 text-xs font-semibold"
            >
              {t(`logs.${k}`)}
            </Button>
          ))}
        </div>
        <div className="flex border border-border rounded-lg overflow-hidden">
          {(['all', 'stream', 'nonStream'] as const).map(k => (
            <Button
              key={k}
              variant={streamMode === k ? "default" : "ghost"}
              size="sm"
              onClick={() => { setStreamMode(k as any); resetPage(); }}
              className="rounded-none h-auto py-1.5 text-xs font-semibold"
            >
              {t(k === 'all' ? 'logs.all' : k === 'stream' ? 'logs.stream' : 'logs.nonStream')}
            </Button>
          ))}
        </div>
        <Input
          value={modelFilter}
          onChange={e => { setModelFilter(e.target.value); resetPage(); }}
          placeholder={t('logs.filterModel')}
          className="w-56 h-9 text-sm"
        />
      </div>

      {error && (
        <p className="text-xs text-destructive bg-destructive/10 border border-destructive/20 rounded-lg px-3 py-2">{error}</p>
      )}

      <div className="bg-card border border-border rounded-xl overflow-hidden">
        <div className="overflow-x-auto">
          <table className="w-full text-xs">
            <thead className="bg-muted/50 text-muted-foreground">
              <tr>
                <th className="text-left px-4 py-2 font-medium">{t('logs.time')}</th>
                <th className="text-left px-3 py-2 font-medium">{t('logs.model')}</th>
                <th className="text-left px-3 py-2 font-medium">{t('logs.account')}</th>
                <th className="text-right px-3 py-2 font-medium">{t('logs.input')}</th>
                <th className="text-right px-3 py-2 font-medium">{t('logs.output')}</th>
                <th className="text-right px-3 py-2 font-medium">{t('logs.cacheTokens', { defaultValue: '缓存' })}</th>
                <th className="text-right px-3 py-2 font-medium">{t('logs.elapsed', { defaultValue: '耗时' })}</th>
                <th className="text-right px-3 py-2 font-medium">{t('logs.ttft')}</th>
                <th className="text-right px-3 py-2 font-medium">{t('logs.throughput')}</th>
                <th className="w-10 px-3 py-2 text-center font-medium">{t('logs.status')}</th>
              </tr>
            </thead>
            <tbody className="divide-y divide-border/50">
              {isLoading && (
                <tr><td colSpan={10} className="px-4 py-10 text-center text-muted-foreground"><Loader2 className="inline animate-spin mr-2" size={14} />{t('logs.loading')}</td></tr>
              )}
              {!isLoading && logs.length === 0 && (
                <tr><td colSpan={10} className="px-4 py-10 text-center text-muted-foreground">{t('logs.empty')}</td></tr>
              )}
              {!isLoading && displayLogs.map(log => (
                <tr
                  key={log.id}
                  onClick={() => openLogDetail(log)}
                  title={log.errorMessage || undefined}
                  className={cn(
                    "cursor-pointer transition-colors",
                    log.success ? "hover:bg-muted/30" : "bg-destructive/[0.06] hover:bg-destructive/10",
                  )}
                >
                  <td className="px-4 py-2 whitespace-nowrap font-mono text-muted-foreground">
                    {new Date(log.timestamp).toLocaleString([], { month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit', second: '2-digit' })}
                  </td>
                  <td className="px-3 py-2 font-mono max-w-[220px] truncate" title={log.model}>{log.model}</td>
                  <td className="px-3 py-2 text-muted-foreground max-w-[140px] truncate" title={log.accountName || ''}>{log.accountName || `#${log.accountId}`}</td>
                  <td className="px-3 py-2 text-right font-mono whitespace-nowrap">{formatK(log.inputTokens)}</td>
                  <td className="px-3 py-2 text-right font-mono whitespace-nowrap">{formatK(log.outputTokens)}</td>
                  <td className="px-3 py-2 text-right font-mono whitespace-nowrap text-muted-foreground">{formatK(cacheOf(log))}</td>
                  <td className="px-3 py-2 text-right font-mono whitespace-nowrap" title={`总耗时 ${fmtSec(log.latencyMs)} · TTFT ${typeof log.ttftMs === 'number' ? fmtSec(log.ttftMs) : '—'}`}>{fmtSec(netMs(log.latencyMs, log.ttftMs, log.isStream))}</td>
                  <td className="px-3 py-2 text-right font-mono whitespace-nowrap">
                    {typeof log.ttftMs === 'number' ? (
                      <span className={cn(log.ttftMs > 1500 ? "text-destructive" : log.ttftMs > 800 ? "text-warning" : "text-foreground")}>{fmtSec(log.ttftMs)}</span>
                    ) : <span className="text-muted-foreground/40">—</span>}
                  </td>
                  <td className="px-3 py-2 text-right font-mono whitespace-nowrap">
                    {typeof log.tps === 'number' ? (
                      <>{log.tps.toFixed(1)} Token/s</>
                    ) : (
                      log.success && log.outputTokens > 0 ? (
                        <>{calcTps(log.outputTokens, log.latencyMs, log.ttftMs).toFixed(1)} Token/s</>
                      ) : <span className="text-muted-foreground/40">—</span>
                    )}
                  </td>
                  <td className="px-3 py-2 text-center">
                    <span title={log.success ? t('logs.success') : (log.errorMessage || t('logs.failed'))} className="inline-flex justify-center">
                      <StatusDot status={log.success ? 'online' : 'offline'} />
                    </span>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
        <div className="flex items-center justify-between px-4 py-2.5 border-t border-border/50">
          <span className="text-xs text-muted-foreground">{t('logs.pageInfo', { page: page + 1, pages: totalPages })}</span>
          <div className="flex items-center gap-2">
            <div className="flex items-center gap-1.5">
              <Input
                value={pageInput}
                onChange={e => setPageInput(e.target.value.replace(/\D/g, ''))}
                onKeyDown={e => { if (e.key === 'Enter' && pageInput) jumpToPage(Number(pageInput)); }}
                placeholder={String(page + 1)}
                className="w-14 h-8 text-center text-xs"
                aria-label={t('logs.goToPage')}
              />
              <Button variant="outline" size="sm" onClick={() => pageInput && jumpToPage(Number(pageInput))}>
                {t('logs.go')}
              </Button>
            </div>
            <Button variant="outline" size="sm" disabled={page === 0 || isLoading} onClick={() => setPage(p => Math.max(0, p - 1))}>
              <ChevronLeft size={14} className="mr-1" />{t('logs.prev')}
            </Button>
            <Button variant="outline" size="sm" disabled={page + 1 >= totalPages || isLoading} onClick={() => setPage(p => p + 1)}>
              {t('logs.next')}<ChevronRight size={14} className="ml-1" />
            </Button>
          </div>
        </div>
      </div>

      <Dialog isOpen={!!detailLog} onClose={closeLogDetail} title={t('dashboard.logDetail.title')} size="lg">
        <div className="space-y-4">
          {detailLog && (
            <div className="rounded-xl border border-border bg-muted/20 overflow-hidden">
              <div className="divide-y divide-border/50 text-xs">
                {[
                  [t('logs.time'), detailLog ? parseServerDate(detailLog.timestamp).toLocaleString() : '—'],
                  [t('logs.model'), detailLog.model],
                  [t('logs.account'), detailLog.accountName || `#${detailLog.accountId}`],
                  [t('logs.elapsed', { defaultValue: '耗时' }), detailLog.latencyMs ? fmtSec(netMs(detailLog.latencyMs, detailLog.ttftMs, detailLog.isStream)) : '—'],
                  [t('logs.ttft'), typeof detailLog.ttftMs === 'number' ? fmtSec(detailLog.ttftMs) : '—'],
                  [t('logs.input'), formatK(detailLog.inputTokens)],
                  [t('logs.output'), formatK(detailLog.outputTokens)],
                  [t('logs.cacheTokens', { defaultValue: '缓存' }), formatK(cacheOf(detailLog))],
                  [t('logs.throughput'), typeof detailLog.tps === 'number' ? `${detailLog.tps.toFixed(1)} Token/s` : (detailLog.success && detailLog.outputTokens > 0 ? `${calcTps(detailLog.outputTokens, detailLog.latencyMs, detailLog.ttftMs).toFixed(1)} Token/s` : '—')],
                  [t('logs.stream'), detailLog.isStream ? t('logs.stream') : t('logs.nonStream')],
                  ['IP', detailData?.client_ip || '—'],
                  [t('logs.status'), detailLog.success ? t('logs.success') : t('logs.failed')],
                ].map(([label, value]) => (
                  <div key={label as string} className="flex items-center justify-between gap-4 px-4 py-2.5">
                    <span className="text-muted-foreground shrink-0">{label}</span>
                    <span className="font-mono font-medium text-foreground text-right truncate flex items-center gap-2 justify-end">
                      {label === t('logs.status') ? (
                        <>
                          <StatusDot status={detailLog.success ? 'online' : 'offline'} />
                          <span className={cn(detailLog.success ? 'text-success' : 'text-destructive')}>{value as string}</span>
                        </>
                      ) : label === t('logs.stream') ? (
                        <Badge variant="secondary" className={cn("text-[10px] px-2 py-0", detailLog.isStream ? "bg-primary/10 text-primary border-primary/20" : "bg-muted text-muted-foreground border-border")}>{value as string}</Badge>
                      ) : label === t('logs.elapsed', { defaultValue: '耗时' }) ? (
                        <span title={`总耗时 ${fmtSec(detailLog.latencyMs)} · TTFT ${typeof detailLog.ttftMs === 'number' ? fmtSec(detailLog.ttftMs) : '—'}`} className="truncate">{value as string}</span>
                      ) : (
                        <span title={String(value)} className="truncate">{value as string}</span>
                      )}
                    </span>
                  </div>
                ))}
              </div>
              {detailLog.errorMessage && !detailLog.success && (
                <div className="px-4 py-3 bg-destructive/5 border-t border-destructive/10">
                  <div className="flex items-start justify-between gap-2">
                    <div className="text-xs font-semibold text-destructive">{t('logs.error')}</div>
                    <Button variant="ghost" size="sm" className="h-7 px-2 text-xs" onClick={copyError}>
                      {copiedError ? <Check size={12} className="mr-1" /> : <Copy size={12} className="mr-1" />}
                      {copiedError ? t('common.copied', { defaultValue: '已复制' }) : t('common.copy', { defaultValue: '复制' })}
                    </Button>
                  </div>
                  <pre className="mt-2 text-xs font-mono whitespace-pre-wrap break-words text-destructive/90 bg-background border border-destructive/20 rounded-lg p-3 max-h-40 overflow-auto">{detailLog.errorMessage}</pre>
                </div>
              )}
            </div>
          )}
          <div className="flex items-center justify-end gap-2">
            <Button variant="outline" size="sm" onClick={expandAllBlocks}>
              <ChevronRight size={14} className="rotate-90" />
              {t('logs.expandAll')}
            </Button>
            <Button variant="outline" size="sm" onClick={collapseAllBlocks}>
              <ChevronRight size={14} />
              {t('logs.collapseAll')}
            </Button>
          </div>
          {(['request', 'response'] as const).map(key => (
            <div key={key}>
              <button
                type="button"
                onClick={() => setBlockOpen(prev => ({ ...prev, [key]: !prev[key] }))}
                className="w-full flex items-center gap-1.5 text-xs font-bold text-muted-foreground uppercase tracking-widest mb-1 hover:text-foreground transition-colors"
              >
                <ChevronRight size={14} className={cn('transition-transform', blockOpen[key] && 'rotate-90')} />
                {t(`dashboard.logDetail.${key}`)}
                <span className="ml-auto font-normal normal-case tracking-normal text-muted-foreground/60">
                  {blockOpen[key] ? '' : '…'}
                </span>
              </button>
              {blockOpen[key] && (
                <div className="bg-muted/50 border border-border rounded-lg p-3 max-h-[50vh] overflow-y-auto overflow-x-hidden">
                  {detailLoading ? '…' : <JsonView key={`${key}-${treeKey}`} text={detailData?.[key === 'request' ? 'request_body' : 'response_body'] || ''} allExpanded={allExpanded} truncatedNotice={t('logs.truncated')} />}
                </div>
              )}
            </div>
          ))}
        </div>
      </Dialog>
    </div>
  );
}
