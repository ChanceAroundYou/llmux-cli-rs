import React, { useCallback, useEffect, useState, useMemo } from 'react';
import { apiFetch } from '../lib/api';
import { useTranslation } from 'react-i18next';
import { useSearchParams } from 'react-router-dom';
import { Loader2, ScrollText, ChevronLeft, ChevronRight } from 'lucide-react';
import { StatusBadge } from '@/components/shared/StatusBadge';
import { Dialog } from '../components/Modal';
import { JsonView } from '@/components/shared/JsonTree';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { Badge } from '@/components/ui/badge';
import { parseServerDate } from '../utils/date';
import { cn } from '@/lib/utils';

// t/s = output*1000 / (latency - ttft); falls back to full latency when ttft is absent.
const calcTps = (outputTokens: number, latencyMs: number, ttftMs: number | null | undefined): number => {
  const gen = Math.max(1, (latencyMs || 0) - (ttftMs ?? 0));
  return ((outputTokens || 0) * 1000) / gen;
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
  const [streamOnly, setStreamOnly] = useState(false);
  const [slowOnly, setSlowOnly] = useState(false);
  const [page, setPage] = useState(0);
  const [total, setTotal] = useState(0);
  const [pageInput, setPageInput] = useState('');
  // 日志详情（发送/收到）——首页跳转经 ?log=<id> 打开
  const [detailLog, setDetailLog] = useState<LogEntry | null>(null);
  const [detailData, setDetailData] = useState<{ request_body: string | null; response_body: string | null; client_ip?: string | null } | null>(null);
  const [detailLoading, setDetailLoading] = useState(false);
  const [searchParams, setSearchParams] = useSearchParams();
  // 详情整块折叠 + JSON 节点全部展开/收起（treeKey 变更触发 remount 重灌初始态）
  const [blockOpen, setBlockOpen] = useState({ request: true, response: true });
  const [allExpanded, setAllExpanded] = useState<boolean | undefined>(undefined);
  const [treeKey, setTreeKey] = useState(0);

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

  // 首页跳转过来（?log=<id>）：自动打开详情
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

  // 客户端快捷过滤：仅流式 / 慢查询（latency>2000 或 ttft>1000）
  const displayLogs = useMemo(() => {
    return logs.filter(l => {
      if (streamOnly && !l.isStream) return false;
      if (slowOnly && !((l.latencyMs || 0) > 2000 || (typeof l.ttftMs === 'number' && l.ttftMs > 1000))) return false;
      return true;
    });
  }, [logs, streamOnly, slowOnly]);

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

      {/* Filters */}
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
        <Input
          value={modelFilter}
          onChange={e => { setModelFilter(e.target.value); resetPage(); }}
          placeholder={t('logs.filterModel')}
          className="w-56 h-9 text-sm"
        />
        <Button
          variant={streamOnly ? "default" : "ghost"}
          size="sm"
          onClick={() => { setStreamOnly(v => !v); resetPage(); }}
          className="h-9 text-xs font-semibold"
        >
          {t('logs.streamOnly')}
        </Button>
        <Button
          variant={slowOnly ? "default" : "ghost"}
          size="sm"
          onClick={() => { setSlowOnly(v => !v); resetPage(); }}
          className="h-9 text-xs font-semibold"
        >
          {t('logs.slowOnly')}
        </Button>
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
                <th className="text-right px-3 py-2 font-medium">{t('logs.tokens')} ({t('logs.input')}/{t('logs.output')})</th>
                <th className="text-right px-3 py-2 font-medium">{t('logs.latency')}</th>
                <th className="text-right px-3 py-2 font-medium">{t('logs.ttft')}</th>
                <th className="text-right px-3 py-2 font-medium">{t('logs.throughput')}</th>
                <th className="text-left px-3 py-2 font-medium">{t('logs.status')}</th>
                <th className="text-left px-3 py-2 font-medium">{t('logs.error')}</th>
              </tr>
            </thead>
            <tbody className="divide-y divide-border/50">
              {isLoading && (
                <tr><td colSpan={9} className="px-4 py-10 text-center text-muted-foreground"><Loader2 className="inline animate-spin mr-2" size={14} />{t('logs.loading')}</td></tr>
              )}
              {!isLoading && logs.length === 0 && (
                <tr><td colSpan={9} className="px-4 py-10 text-center text-muted-foreground">{t('logs.empty')}</td></tr>
              )}
              {!isLoading && displayLogs.map(log => (
                <tr key={log.id} onClick={() => openLogDetail(log)} className="hover:bg-muted/30 cursor-pointer">
                  <td className="px-4 py-2 whitespace-nowrap font-mono text-muted-foreground">
                    {new Date(log.timestamp).toLocaleString([], { month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit', second: '2-digit' })}
                  </td>
                  <td className="px-3 py-2 font-mono max-w-[220px] truncate" title={log.model}>{log.model}</td>
                  <td className="px-3 py-2 text-muted-foreground">{log.accountName || `#${log.accountId}`}</td>
                  <td className="px-3 py-2 text-right font-mono whitespace-nowrap">{log.inputTokens}/{log.outputTokens}</td>
                  <td className="px-3 py-2 text-right font-mono whitespace-nowrap">{log.latencyMs}ms</td>
                  <td className="px-3 py-2 text-right font-mono whitespace-nowrap">
                    {typeof log.ttftMs === 'number' ? (
                      <span className={log.ttftMs > 1500 ? "text-destructive" : log.ttftMs > 800 ? "text-warning" : "text-foreground"}>{log.ttftMs}ms</span>
                    ) : <span className="text-muted-foreground/40">—</span>}
                  </td>
                  <td className="px-3 py-2 text-right font-mono whitespace-nowrap">
                    {typeof log.tps === 'number' ? (
                      <>{log.tps.toFixed(1)} t/s</>
                    ) : (
                      log.success && log.outputTokens > 0 ? (
                        <>{calcTps(log.outputTokens, log.latencyMs, log.ttftMs).toFixed(1)} t/s</>
                      ) : <span className="text-muted-foreground/40">—</span>
                    )}
                  </td>
                  <td className="px-3 py-2"><StatusBadge status={log.success ? 'online' : 'offline'} label={log.success ? t('logs.success') : t('logs.failed')} /></td>
                  <td className="px-3 py-2 max-w-[280px]">
                    {log.errorMessage ? (
                      <span className="text-destructive block truncate" title={log.errorMessage}>{log.errorMessage}</span>
                    ) : (
                      <span className="text-muted-foreground/40">—</span>
                    )}
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

      {/* 日志详情（发送/收到） */}
      <Dialog isOpen={!!detailLog} onClose={closeLogDetail} title={t('dashboard.logDetail.title')} size="lg">
        <div className="space-y-4">
          <div className="flex flex-wrap gap-x-4 gap-y-1 text-xs text-muted-foreground">
            <span>{detailLog && parseServerDate(detailLog.timestamp).toLocaleString()}</span>
            <span className="font-semibold text-foreground">{detailLog?.model}</span>
            {detailLog?.accountName && <span>{detailLog.accountName}</span>}
            {detailLog && <span>{detailLog.success ? '200' : 'ERR'}</span>}
            {detailLog && <span>{t('logs.ttft')}: <span className="font-mono">{typeof detailLog.ttftMs === 'number' ? `${detailLog.ttftMs}ms` : '—'}</span></span>}
            {detailLog && <span>{t('logs.total')}: <span className="font-mono">{(detailLog.latencyMs / 1000).toFixed(1)}s</span></span>}
            {detailLog && <span>{t('logs.throughput')}: <span className="font-mono">{typeof detailLog.tps === 'number' ? `${detailLog.tps.toFixed(1)} t/s` : (detailLog.success && detailLog.outputTokens > 0 ? `${calcTps(detailLog.outputTokens, detailLog.latencyMs, detailLog.ttftMs).toFixed(1)} t/s` : '—')}</span></span>}
            {detailLog?.isStream ? <Badge variant="secondary" className="bg-primary/10 text-primary border-primary/20">{t('logs.stream')}</Badge> : <Badge variant="secondary" className="bg-muted text-muted-foreground border-border">{t('logs.nonStream')}</Badge>}
            {detailData?.client_ip && <span>IP: <span className="font-mono">{detailData.client_ip}</span></span>}
          </div>
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
