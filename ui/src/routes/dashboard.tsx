import React, { useEffect, useState, useMemo, lazy, Suspense } from 'react';
import { apiFetch } from "@/lib/api";
import { useTranslation } from 'react-i18next';
import { useNavigate } from 'react-router-dom';
import { AlertTriangle, Database, Users, Zap, Key, Shield, Activity, LayoutDashboard, ChevronDown, Search } from 'lucide-react';
import { parseServerDate } from '../utils/date';
import { cn } from '../lib/utils'
import { StatusDot } from '../components/shared/StatusDot'
import { PageHeader } from '../components/shared/PageHeader'
import { EmptyState } from '../components/shared/EmptyState'
import { StatCard } from '../components/shared/StatCard'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Badge } from '@/components/ui/badge'
import type { ChartData, ChartOptions } from 'chart.js'

const PulseChart = lazy(() => import('@/components/Dashboard/PulseChart'))

interface ProviderHealth { id: string; name?: string; status: string; totalChecks: number; }
interface ActivityEntry { id: number; timestamp: number; model: string; success: number; latency_ms: number; error_message: string | null; account_name: string; output_tokens: number; ttft_ms: number | null; is_stream: number; }
interface ModelHealthEntry { model: string; success: number; latency: number; error: string | null; last_checked: number; account_name: string; account_id: number; provider_id: string; }
interface ModelAlias { id: number; alias: string; target_model: string; provider_id: string | null; }
interface AggregateAlias { id: number; alias: string; candidates: { account_id: number; model: string }[]; interval_secs: number; active: number; last_status: (boolean | null)[]; pending_target: number | null; confirm_count: number; }

// t/s = output*1000 / (latency - ttft); falls back to full latency when ttft is absent.
const calcTps = (outputTokens: number, latencyMs: number, ttftMs: number | null | undefined): number => {
  const gen = Math.max(1, (latencyMs || 0) - (ttftMs ?? 0));
  return ((outputTokens || 0) * 1000) / gen;
};

export default function Dashboard() {
  const { t } = useTranslation();
  const navigate = useNavigate();

  const [accountCount, setAccountCount] = useState(0);
  const [aliasCount, setAliasCount] = useState(0);
  const [keyCount, setKeyCount] = useState(0);
  const [healthyCount, setHealthyCount] = useState(0);
  // Collapsible dashboard panels
  const [aliasCollapsed, setAliasCollapsed] = useState(false);
  const [logsCollapsed, setLogsCollapsed] = useState(false);
  const [aliases, setAliases] = useState<ModelAlias[]>([]);
  const [aggregateAliases, setAggregateAliases] = useState<AggregateAlias[]>([]);
  const [modelHealth, setModelHealth] = useState<ModelHealthEntry[]>([]);
  const [activityLogs, setActivityLogs] = useState<ActivityEntry[]>([]);
  const [isLoading, setIsLoading] = useState(true);
  const [searchQuery, setSearchQuery] = useState('');
  const [onlyShowErrors, setOnlyShowErrors] = useState(false);

  const loadAll = async () => {
    setIsLoading(true);
    const ac = new AbortController();
    const sig = ac.signal;
    const applyDashboard = (data: any) => {
      const accounts = Array.isArray(data.accounts) ? data.accounts : [];
      const aliasData: ModelAlias[] = Array.isArray(data.aliases) ? data.aliases : [];
      const aggData: AggregateAlias[] = Array.isArray(data.aggregateAliases) ? data.aggregateAliases : [];
      const health: ProviderHealth[] = Array.isArray(data.health) ? data.health : [];
      const mh: ModelHealthEntry[] = Array.isArray(data.modelHealth) ? data.modelHealth : [];
      const actEntries: ActivityEntry[] = Array.isArray(data.activity?.entries) ? data.activity.entries : (Array.isArray(data.activity) ? data.activity : []);
      setAccountCount(accounts.length);
      setAliasCount(aliasData.length + aggData.length);
      setHealthyCount(health.filter(h => h.status !== 'down' && h.status !== 'unknown').length);
      setAliases(aliasData);
      setAggregateAliases(aggData);
      setModelHealth(mh);
      setActivityLogs(actEntries);
      if (typeof data.keysCount === 'number') setKeyCount(data.keysCount);
      else if (Array.isArray(data.keys)) setKeyCount(data.keys.length);
    };
    try {
      const dashRes = await apiFetch('/api/dashboard', { signal: sig });
      if (dashRes.ok) {
        const data = await dashRes.json();
        applyDashboard(data);
        setIsLoading(false);
        return () => ac.abort();
      }
      // Fallback: legacy fan-out (keeps working against old backends)
      const [accRes, aliasRes, aggRes, healthRes, mhRes] = await Promise.all([
        apiFetch('/api/accounts', { signal: sig }), apiFetch('/api/models/aliases', { signal: sig }), apiFetch('/api/aggregate-aliases', { signal: sig }),
        apiFetch('/api/health', { signal: sig }), apiFetch('/api/models/health', { signal: sig }),
      ]);
      const accounts = accRes.ok ? await accRes.json() : [];
      const aliasData: ModelAlias[] = aliasRes.ok ? await aliasRes.json() : [];
      const aggData: AggregateAlias[] = aggRes.ok ? await aggRes.json() : [];
      const health: ProviderHealth[] = healthRes.ok ? await healthRes.json() : [];
      const mh: ModelHealthEntry[] = mhRes.ok ? await mhRes.json() : [];
      setAccountCount(accounts.length);
      setAliasCount(aliasData.length + aggData.length);
      setHealthyCount(health.filter(h => h.status !== 'down' && h.status !== 'unknown').length);
      setAliases(aliasData);
      setAggregateAliases(Array.isArray(aggData) ? aggData : []);
      setModelHealth(mh);
      setIsLoading(false);
      const [actRes, keyRes] = await Promise.all([
        apiFetch('/api/activity?limit=100', { signal: sig }), apiFetch('/api/keys', { signal: sig }),
      ]);
      if (actRes.ok) {
        const actData = await actRes.json();
        setActivityLogs(actData.entries || []);
      }
      if (keyRes.ok) {
        const keys = await keyRes.json();
        setKeyCount(Array.isArray(keys) ? keys.length : 0);
      }
    } catch (err: any) {
      if (err?.name === 'AbortError') return;
      console.error('Dashboard load failed:', err);
      setIsLoading(false);
    }
    return () => ac.abort();
  };

  useEffect(() => { const c = loadAll(); return () => { (c as any)?.then?.((f: any) => f?.()); }; }, []);

  const aliasHealthList = useMemo(() => {
    // 普通别名：按 target_model 找最优的一条 model health
    const bestByModel = new Map<string, ModelHealthEntry>();
    modelHealth.forEach(h => {
      const ex = bestByModel.get(h.model);
      if (!ex || (h.success && h.latency < ex.latency) || (!ex.success && h.success)) {
        bestByModel.set(h.model, h);
      }
    });
    // 聚合别名：按候选 (account_id, model) 多键取最优（不同候选可能落在不同账号/模型行）
    const bestByCandidate = new Map<string, ModelHealthEntry>();
    modelHealth.forEach(h => {
      const key = `${h.account_id}::${h.model}`;
      const ex = bestByCandidate.get(key);
      if (!ex || (h.success && h.latency < ex.latency) || (!ex.success && h.success)) {
        bestByCandidate.set(key, h);
      }
    });
    const ordinary = aliases.map(a => {
      const h = bestByModel.get(a.target_model);
      return {
        alias: a.alias, target_model: a.target_model, provider: a.provider_id || '', kind: 'ordinary' as const,
        success: h ? h.success === 1 : false, latency: h?.latency ?? null, lastChecked: h?.last_checked ?? null,
      };
    });
    const aggregate = aggregateAliases.map(agg => {
      const activeIdx = Math.min(agg.active ?? 0, Math.max(0, agg.candidates.length - 1));
      const activeCand = agg.candidates[activeIdx];
      let h: ModelHealthEntry | undefined;
      if (activeCand) {
        h = bestByCandidate.get(`${activeCand.account_id}::${activeCand.model}`);
        if (!h) h = bestByModel.get(activeCand.model);
      }
      // 兜底：所有候选中取最新一条
      if (!h && agg.candidates.length) {
        let best: ModelHealthEntry | undefined;
        for (const c of agg.candidates) {
          const cand = bestByCandidate.get(`${c.account_id}::${c.model}`) || bestByModel.get(c.model);
          if (cand && (!best || (cand.last_checked ?? 0) > (best.last_checked ?? 0))) best = cand;
        }
        h = best;
      }
      const label = activeCand ? `${activeCand.model} (#${activeIdx + 1}/${agg.candidates.length} 活跃)` : `${agg.candidates.length} 候选`;
      return {
        alias: agg.alias, target_model: label, provider: 'aggregate', kind: 'aggregate' as const,
        success: h ? h.success === 1 : false, latency: h?.latency ?? null, lastChecked: h?.last_checked ?? null,
      };
    });
    return [...ordinary, ...aggregate];
  }, [aliases, aggregateAliases, modelHealth]);

  const filteredAliases = useMemo(() => {
    if (!searchQuery) return aliasHealthList;
    const q = searchQuery.toLowerCase();
    return aliasHealthList.filter(a => a.alias.toLowerCase().includes(q) || a.target_model.toLowerCase().includes(q) || a.provider.toLowerCase().includes(q));
  }, [aliasHealthList, searchQuery]);

  const filteredLogs = useMemo(() => {
    const src = onlyShowErrors ? activityLogs.filter(l => l.success !== 1) : activityLogs;
    return src.slice(0, 100);
  }, [activityLogs, onlyShowErrors]);

  const logMetrics = useMemo(() => {
    const recent = activityLogs.slice(0, 100);
    const ok = recent.filter(l => l.success === 1).length;
    const rate = recent.length ? Math.round((ok / recent.length) * 100) : 0;
    const avg = recent.length ? Math.round(recent.reduce((a, l) => a + (l.latency_ms || 0), 0) / recent.length) : 0;
    const withTtft = recent.filter(l => typeof l.ttft_ms === 'number');
    const avgTtft = withTtft.length ? Math.round(withTtft.reduce((a, l) => a + (l.ttft_ms as number), 0) / withTtft.length) : 0;
    const latencies = recent.map(l => l.latency_ms || 0).sort((a, b) => a - b);
    const p95 = latencies.length ? latencies[Math.min(latencies.length - 1, Math.floor(latencies.length * 0.95))] : 0;
    const withOut = recent.filter(l => l.success === 1 && l.output_tokens > 0);
    const avgTps = withOut.length ? Math.round(withOut.reduce((a, l) => a + calcTps(l.output_tokens, l.latency_ms, l.ttft_ms), 0) / withOut.length) : 0;
    return { rate, avg, avgTtft, p95, avgTps, len: recent.length };
  }, [activityLogs]);

  const pulseChartData: ChartData<'bar'> = useMemo(() => {
    const displayLogs = [...activityLogs.slice(0, 100)].reverse();
    return {
      labels: displayLogs.map(() => ''),
      datasets: [{
        data: displayLogs.map(l => {
          if (l.success !== 1) return 0.8;
          return (l.latency_ms || 0) > 2000 ? 1.2 : 1;
        }),
        backgroundColor: displayLogs.map(l => {
          if (l.success !== 1) return '#ef4444';
          return (l.latency_ms || 0) > 2000 ? '#f59e0b' : '#22c55e';
        }),
        borderRadius: 0,
        barThickness: 3,
      }]
    };
  }, [activityLogs]);

  const pulseChartOptions: ChartOptions<'bar'> = useMemo(() => ({
    responsive: true,
    maintainAspectRatio: false,
    animation: false,
    plugins: {
      legend: { display: false },
      tooltip: {
        enabled: true,
        backgroundColor: '#1e293b',
        titleFont: { size: 10 },
        bodyFont: { size: 10 },
        displayColors: false,
        callbacks: {
          label: (context) => {
            const logs = [...activityLogs.slice(0, 100)].reverse();
            const log = logs[context.dataIndex];
            if (!log) return '';
            if (log.success !== 1) return ` Error: ${log.error_message || 'ERR'}`;
            return ` ${log.latency_ms}ms`;
          }
        }
      }
    },
    scales: {
      x: { display: false },
      y: {
        display: false,
        beginAtZero: true,
        max: 1.5
      }
    }
  }), [activityLogs]);

  const ttftChartData: ChartData<'bar'> = useMemo(() => {
    const displayLogs = [...activityLogs.slice(0, 100)].reverse();
    return {
      labels: displayLogs.map(() => ''),
      datasets: [{
        data: displayLogs.map(l => {
          if (l.success !== 1) return 0.8;
          const ttft = typeof l.ttft_ms === 'number' ? l.ttft_ms : 0;
          return ttft > 1500 ? 1.2 : 1;
        }),
        backgroundColor: displayLogs.map(l => {
          if (l.success !== 1) return '#ef4444';
          const ttft = typeof l.ttft_ms === 'number' ? l.ttft_ms : 0;
          return ttft > 1500 ? '#ef4444' : ttft > 800 ? '#f59e0b' : '#22c55e';
        }),
        borderRadius: 0,
        barThickness: 3,
      }]
    };
  }, [activityLogs]);

  const ttftChartOptions: ChartOptions<'bar'> = useMemo(() => ({
    responsive: true,
    maintainAspectRatio: false,
    animation: false,
    plugins: {
      legend: { display: false },
      tooltip: {
        enabled: true,
        backgroundColor: '#1e293b',
        titleFont: { size: 10 },
        bodyFont: { size: 10 },
        displayColors: false,
        callbacks: {
          label: (context) => {
            const logs = [...activityLogs.slice(0, 100)].reverse();
            const log = logs[context.dataIndex];
            if (!log) return '';
            if (log.success !== 1) return ` Error: ${log.error_message || 'ERR'}`;
            const ttft = typeof log.ttft_ms === 'number' ? log.ttft_ms : 0;
            return ` TTFT: ${ttft}ms`;
          }
        }
      }
    },
    scales: {
      x: { display: false },
      y: {
        display: false,
        beginAtZero: true,
        max: 1.5
      }
    }
  }), [activityLogs]);

  const providerList = useMemo(() => {
    const set = new Set(aliasHealthList.map(a => a.provider).filter(Boolean));
    return ['All', ...Array.from(set)];
  }, [aliasHealthList]);

  const [selectedProvider, setSelectedProvider] = useState('All');
  const displayAliases = useMemo(() => {
    if (selectedProvider === 'All') return filteredAliases;
    return filteredAliases.filter(a => a.provider === selectedProvider);
  }, [filteredAliases, selectedProvider]);

  return (
    <div className="flex flex-col gap-6 animate-fadeIn pb-5 lg:h-[calc(100vh-126px)] lg:flex-1 lg:min-h-0">

      {/* Header */}
      <PageHeader
        icon={<LayoutDashboard size={24} />}
        title={t('common.dashboard')}
        subtitle={t('dashboard.subtitle')}
      />

      {/* 4 Stat Cards */}
      <section className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-4 gap-6">
        <StatCard label={t('dashboard.stats.accounts')} value={accountCount} subtitle={`${t('dashboard.healthy')}: ${healthyCount}`} icon={Users} trend={{ value: healthyCount, label: t('dashboard.healthy'), type: 'primary' }} />
        <StatCard label={t('dashboard.stats.aliases')} value={aliasCount} subtitle={`${t('dashboard.stats.aliasesHint')}: ${aliasCount}`} icon={Zap} trend={{ value: aliasCount, type: 'warning' }} />
        <StatCard label={t('dashboard.stats.apiKeys')} value={keyCount} subtitle={t('dashboard.stats.keysHint')} icon={Key} trend={{ value: keyCount, type: 'primary' }} />
        <StatCard label={t('dashboard.stats.healthy')} value={healthyCount} subtitle={`${accountCount > 0 ? Math.round((healthyCount / accountCount) * 100) : 0}% ${t('dashboard.online')}`} icon={Shield} trend={{ value: `${accountCount > 0 ? Math.round((healthyCount / accountCount) * 100) : 0}%`, type: 'success' }} />
      </section>

      {/* Two-Column Panel */}
      <div className="grid grid-cols-1 lg:grid-cols-12 gap-6 lg:gap-8 lg:items-stretch lg:flex-1 lg:min-h-0">

        {/* Left: Alias Health — 7 cols */}
        <section className={cn("lg:col-span-7 bg-card border border-border rounded-xl shadow-sm lg:overflow-hidden lg:flex lg:flex-col", aliasCollapsed && "lg:self-start")}>
          <div className="p-6 border-b border-border/50">
            <div className="flex justify-between items-center flex-wrap gap-2">
              <div className="flex items-center gap-2">
                <Database size={16} className="text-muted-foreground" />
                <h2 className="text-lg font-bold text-foreground">{t('dashboard.aliasHealth')}</h2>
              </div>
              <div className="flex items-center gap-2">
                <Badge variant="secondary" className="bg-success/10 text-success border-success/20 hover:bg-success/20">
                  {aliasHealthList.filter(a => a.success).length}/{aliasHealthList.length} OK
                </Badge>
                <button
                  type="button"
                  aria-label={aliasCollapsed ? 'Expand alias health' : 'Collapse alias health'}
                  onClick={() => setAliasCollapsed(v => !v)}
                  className="p-1.5 rounded-md text-muted-foreground hover:bg-muted hover:text-foreground transition-colors"
                >
                  <ChevronDown size={16} className={cn('transition-transform duration-200', aliasCollapsed && '-rotate-90')} />
                </button>
              </div>
            </div>

            {!aliasCollapsed && (<>
            <div className="mt-4 space-y-3">
              <div className="relative">
                <Search className="absolute left-3 top-1/2 -translate-y-1/2 text-muted-foreground z-10" size={14} />
                <Input
                  type="text" placeholder={t('dashboard.filterAliases')} value={searchQuery}
                  onChange={e => setSearchQuery(e.target.value)}
                  className="pl-9"
                />
              </div>
              {providerList.length > 2 && (
                <div className="flex items-center gap-1.5 overflow-x-auto py-1 text-xs">
                  <span className="text-muted-foreground shrink-0 font-medium mr-1">{t('dashboard.providerFilter')}:</span>
                  {providerList.map(prov => (
                    <Button key={prov}
                      variant={selectedProvider === prov ? "default" : "ghost"}
                      size="sm"
                      onClick={() => setSelectedProvider(prov)}
                      className="h-auto px-2.5 py-1 text-xs font-medium shrink-0"
                    >{prov === 'All' ? t('dashboard.all') : prov}</Button>
                  ))}
                </div>
              )}
            </div>
            </>)}
          </div>

          {!aliasCollapsed && (<>
          <div className="divide-y divide-border/50 lg:flex-1 lg:overflow-y-auto">
            {isLoading ? (
              <div className="p-6 space-y-3">
                {Array.from({ length: 4 }).map((_, i) => (
                  <div key={i} className="h-14 rounded-lg bg-muted/40 animate-pulse" />
                ))}
              </div>
            ) : displayAliases.length === 0 ? (
              <EmptyState icon={Database} title={t('dashboard.noAliases')} />
            ) : displayAliases.map((a, i) => (
              <div key={i} className="p-5 flex items-center justify-between gap-4 hover:bg-muted/30 transition-colors duration-200">
                <div className="flex items-center gap-4 min-w-0">
                  <StatusDot status={a.success ? "online" : a.lastChecked ? "offline" : "unknown"} />
                  <div className="min-w-0">
                    <div className="flex items-center gap-2 flex-wrap">
                      <span className="font-semibold text-foreground tracking-tight truncate">{a.alias}</span>
                      {a.provider && (
                        <span className="text-xs bg-muted text-muted-foreground font-medium px-2 py-0.5 rounded border border-border">{a.provider}</span>
                      )}
                    </div>
                    <p className="text-xs text-muted-foreground font-mono mt-1">{a.target_model}</p>
                  </div>
                </div>
                <div className="text-right shrink-0">
                  {a.latency !== null ? (
                    <Badge variant={a.success ? (a.latency < 500 ? "secondary" : a.latency < 1200 ? "secondary" : "secondary") : "destructive"}
                      className={cn(
                        "font-mono",
                        a.success
                          ? a.latency < 500 ? 'bg-success/10 text-success border-success/20' : a.latency < 1200 ? 'bg-warning/10 text-warning border-warning/20' : 'bg-primary/10 text-primary border-primary/20'
                          : 'bg-destructive/10 text-destructive border-destructive/20'
                      )}
                    >
                      {a.success ? `${(a.latency / 1000).toFixed(1)}s` : 'ERR'}
                    </Badge>
                  ) : a.lastChecked ? (
                    <Badge variant="destructive" className="bg-destructive/10 text-destructive border-destructive/20 font-mono">ERR</Badge>
                  ) : (
                    <span className="text-xs text-muted-foreground/60">{t('dashboard.untested')}</span>
                  )}
                </div>
              </div>
            ))}
          </div>

          <div className="p-4 bg-muted border-t border-border/50 text-xs text-muted-foreground flex justify-between">
            <span>{t('dashboard.healthSource')}</span>
            <span>{aliasHealthList.filter(a => a.lastChecked).length} {t('dashboard.tested')}</span>
          </div>
            </>)}
        </section>

        {/* Right: Activity Log — 5 cols */}
        <section className={cn("lg:col-span-5 bg-card border border-border rounded-xl shadow-sm lg:overflow-hidden lg:flex lg:flex-col", logsCollapsed && "lg:self-start")}>
          <div className="p-6 border-b border-border/50">
            <div className="flex flex-col sm:flex-row justify-between items-start sm:items-center gap-3">
              <div className="flex items-center gap-2">
                <Activity size={16} className="text-muted-foreground" />
                <h2 className="text-lg font-bold text-foreground">{t('dashboard.recentLogs')}</h2>
              </div>
              <div className="flex items-center gap-2">
                <button
                  type="button"
                  onClick={() => setOnlyShowErrors(!onlyShowErrors)}
                  className={cn(
                    "inline-flex items-center rounded-full border px-2.5 py-0.5 text-xs font-semibold transition-colors focus:outline-none focus:ring-2 focus:ring-ring focus:ring-offset-2",
                    onlyShowErrors
                      ? "bg-destructive/10 text-destructive border-destructive/20 hover:bg-destructive/20"
                      : "bg-muted text-muted-foreground border-border hover:bg-muted/80"
                  )}
                >
                  {onlyShowErrors ? t('dashboard.showAll') : t('dashboard.errorsOnly')}
                </button>
                <button
                  type="button"
                  aria-label={logsCollapsed ? 'Expand activity log' : 'Collapse activity log'}
                  onClick={() => setLogsCollapsed(v => !v)}
                  className="p-1.5 rounded-md text-muted-foreground hover:bg-muted hover:text-foreground transition-colors"
                >
                  <ChevronDown size={16} className={cn('transition-transform duration-200', logsCollapsed && '-rotate-90')} />
                </button>
              </div>
            </div>

            {!logsCollapsed && (<>
            {/* KPI strip */}
            <div className="grid grid-cols-3 gap-3 mt-6 p-4 bg-muted rounded-xl border border-border/50">
              <div>
                <span className="text-[10px] font-medium text-muted-foreground block uppercase tracking-wider">{t('dashboard.monitor.avgTtft')}</span>
                <div className="flex items-baseline gap-1 mt-1">
                  <span className="text-xl font-bold text-foreground tracking-tight">{logMetrics.avgTtft}ms</span>
                </div>
              </div>
              <div>
                <span className="text-[10px] font-medium text-muted-foreground block uppercase tracking-wider">{t('dashboard.monitor.p95Latency')}</span>
                <div className="flex items-baseline gap-1 mt-1">
                  <span className="text-xl font-bold text-foreground tracking-tight">{logMetrics.p95}ms</span>
                </div>
              </div>
              <div>
                <span className="text-[10px] font-medium text-muted-foreground block uppercase tracking-wider">{t('dashboard.monitor.avgTps')}</span>
                <div className="flex items-baseline gap-1 mt-1">
                  <span className="text-xl font-bold text-foreground tracking-tight">{logMetrics.avgTps}<span className="text-xs font-normal text-muted-foreground ml-0.5">t/s</span></span>
                </div>
              </div>
            </div>

            <div className="grid grid-cols-2 gap-4 mt-4 p-4 bg-muted rounded-xl border border-border/50">
              <div>
                <span className="text-xs font-medium text-muted-foreground block uppercase tracking-wider">{t('dashboard.monitor.successRate')}</span>
                <div className="flex items-baseline gap-1 mt-1">
                  <span className="text-xl font-bold text-foreground tracking-tight">{logMetrics.rate}%</span>
                </div>
              </div>
              <div>
                <span className="text-xs font-medium text-muted-foreground block uppercase tracking-wider">{t('dashboard.monitor.avgLag')}</span>
                <div className="flex items-baseline gap-1 mt-1">
                  <span className="text-xl font-bold text-foreground tracking-tight">{(logMetrics.avg / 1000).toFixed(1)}s</span>
                </div>
              </div>
            </div>

            {/* Latency Pulse — chart.js lazy-loaded */}
            <div className="mt-4 space-y-2">
              <div className="flex items-center justify-between">
                <span className="text-[10px] font-bold text-muted-foreground uppercase tracking-widest">{t('dashboard.monitor.latencyPulse')}</span>
                <span className="text-[10px] font-mono text-muted-foreground/60">{activityLogs.length} {t('dashboard.monitor.reqs')}</span>
              </div>
              <div className="h-16 w-full bg-muted/20 rounded-lg p-2 border border-border/40 relative">
                {activityLogs.length > 0 ? (
                  <Suspense fallback={<div className="h-full w-full animate-pulse bg-muted/40 rounded" />}>
                    <PulseChart data={pulseChartData} options={pulseChartOptions} />
                  </Suspense>
                ) : (
                  <div className="h-full flex items-center justify-center text-[10px] text-muted-foreground/30 italic">{t('dashboard.noActivity')}</div>
                )}
              </div>
            </div>

            {/* TTFT Pulse — warm colors, thresholds 800/1500ms */}
            <div className="mt-4 space-y-2">
              <div className="flex items-center justify-between">
                <span className="text-[10px] font-bold text-muted-foreground uppercase tracking-widest">{t('dashboard.monitor.ttftPulse')}</span>
                <span className="text-[10px] font-mono text-muted-foreground/60">&lt;800ms #22c55e · 800–1500ms #f59e0b · &gt;1500ms #ef4444</span>
              </div>
              <div className="h-16 w-full bg-muted/20 rounded-lg p-2 border border-border/40 relative">
                {activityLogs.length > 0 ? (
                  <Suspense fallback={<div className="h-full w-full animate-pulse bg-muted/40 rounded" />}>
                    <PulseChart data={ttftChartData} options={ttftChartOptions} />
                  </Suspense>
                ) : (
                  <div className="h-full flex items-center justify-center text-[10px] text-muted-foreground/30 italic">{t('dashboard.noActivity')}</div>
                )}
              </div>
            </div>
            </>)}
          </div>

          {!logsCollapsed && (<>
          <div className="p-6 space-y-3 lg:flex-1 lg:overflow-y-auto">
            {filteredLogs.length === 0 ? (
              <EmptyState icon={AlertTriangle} title={onlyShowErrors ? t('dashboard.noErrors') : t('dashboard.noActivity')} />
            ) : filteredLogs.map(log => (
              <div key={log.id}
                onClick={() => navigate(`/logs?log=${log.id}`)}
                className={`p-3 rounded-xl border text-xs transition-colors duration-150 cursor-pointer hover:border-primary/30 ${log.success !== 1
                  ? 'bg-destructive/5 border-destructive/10 hover:bg-destructive/10'
                  : 'bg-muted border-border/50 hover:bg-muted/80'}`}
              >
                <div className="flex items-center gap-2 min-w-0 whitespace-nowrap">
                  <span className="text-muted-foreground/60 shrink-0">
                    {parseServerDate(log.timestamp).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', second: '2-digit', hour12: false })}
                  </span>
                  <span className="font-semibold text-foreground truncate">{log.model}</span>
                  {log.account_name && <span className="text-muted-foreground/60 truncate">{log.account_name}</span>}
                  {log.success !== 1 && log.error_message && (
                    <span className="text-destructive truncate" title={log.error_message}>{log.error_message}</span>
                  )}
                  {typeof log.ttft_ms === 'number' && (
                    <span className={cn("font-mono shrink-0", log.ttft_ms > 1500 ? "text-destructive" : log.ttft_ms > 800 ? "text-warning" : "text-muted-foreground/60")} title="time to first token">
                      TTFT {log.ttft_ms}ms
                    </span>
                  )}
                  {log.success === 1 && log.output_tokens > 0 && (
                    <span className="font-mono text-muted-foreground/60 shrink-0" title="tokens per second">
                      {calcTps(log.output_tokens, log.latency_ms, log.ttft_ms).toFixed(0)} t/s
                    </span>
                  )}
                  <span className="text-muted-foreground shrink-0 ml-auto">{log.latency_ms ? `${(log.latency_ms / 1000).toFixed(1)}s` : '--'}</span>
                  <Badge variant={log.success === 1 ? "secondary" : "destructive"} className={log.success === 1 ? 'bg-success/20 text-success border-success/20' : 'bg-destructive/20 text-destructive border-destructive/20'}>
                    {log.success === 1 ? '200' : 'ERR'}
                  </Badge>
                </div>
              </div>
            ))}
          </div>

          <div className="p-4 bg-muted border-t border-border/50 flex justify-between items-center text-xs">
            <span className="text-muted-foreground/70">{t('dashboard.logCount', { count: activityLogs.length })}</span>
          </div>
            </>)}
        </section>
      </div>
    </div>
  );
}
