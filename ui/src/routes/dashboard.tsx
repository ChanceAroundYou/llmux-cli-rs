import React, { useEffect, useState, useMemo } from 'react';
import { useTranslation } from 'react-i18next';
import { useNavigate } from 'react-router-dom';
import { RefreshCw, Plus, Search, AlertTriangle, Database, Users, Zap, Key, Shield, Activity, LayoutDashboard } from 'lucide-react';
import { parseServerDate } from '../utils/date';
import { cn } from '../lib/utils'
import { StatusDot } from '../components/shared/StatusDot'
import { PageHeader } from '../components/shared/PageHeader'
import { EmptyState } from '../components/shared/EmptyState'
import { StatCard } from '../components/shared/StatCard'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Badge } from '@/components/ui/badge'
import {
  Chart as ChartJS,
  CategoryScale,
  LinearScale,
  BarElement,
  Tooltip,
  type ChartData,
  type ChartOptions,
} from 'chart.js'
import { Bar } from 'react-chartjs-2'

ChartJS.register(CategoryScale, LinearScale, BarElement, Tooltip)

interface ProviderHealth { id: string; name?: string; status: string; totalChecks: number; }
interface ActivityEntry { id: number; timestamp: number; model: string; success: number; latency_ms: number; error_message: string | null; account_name: string; }
interface ModelHealthEntry { model: string; success: number; latency: number; error: string | null; last_checked: number; account_name: string; }
interface ModelAlias { id: number; alias: string; target_model: string; provider_id: string | null; }

export default function Dashboard() {
  const { t } = useTranslation();
  const navigate = useNavigate();

  const [accountCount, setAccountCount] = useState(0);
  const [aliasCount, setAliasCount] = useState(0);
  const [keyCount, setKeyCount] = useState(0);
  const [healthyCount, setHealthyCount] = useState(0);
  const [aliases, setAliases] = useState<ModelAlias[]>([]);
  const [modelHealth, setModelHealth] = useState<ModelHealthEntry[]>([]);
  const [activityLogs, setActivityLogs] = useState<ActivityEntry[]>([]);
  const [isLoading, setIsLoading] = useState(true);
  const [searchQuery, setSearchQuery] = useState('');
  const [onlyShowErrors, setOnlyShowErrors] = useState(false);

  const loadAll = async () => {
    setIsLoading(true);
    try {
      const [accRes, aliasRes, keyRes, healthRes, mhRes, actRes] = await Promise.all([
        fetch('/api/accounts'), fetch('/api/models/aliases'), fetch('/api/keys'),
        fetch('/api/health'), fetch('/api/models/health'), fetch('/api/activity?limit=200'),
      ]);
      const accounts = accRes.ok ? await accRes.json() : [];
      const aliasData: ModelAlias[] = aliasRes.ok ? await aliasRes.json() : [];
      const keys = keyRes.ok ? await keyRes.json() : [];
      const health: ProviderHealth[] = healthRes.ok ? await healthRes.json() : [];
      const mh: ModelHealthEntry[] = mhRes.ok ? await mhRes.json() : [];
      const actData = actRes.ok ? await actRes.json() : { entries: [] };
      setAccountCount(accounts.length);
      setAliasCount(aliasData.length);
      setKeyCount(keys.length);
      setHealthyCount(health.filter(h => h.status !== 'down' && h.status !== 'unknown').length);
      setAliases(aliasData);
      setModelHealth(mh);
      setActivityLogs(actData.entries || []);
    } catch (err) { console.error('Dashboard load failed:', err); }
    finally { setIsLoading(false); }
  };

  useEffect(() => { loadAll(); }, []);

  const aliasHealthList = useMemo(() => {
    const bestByModel = new Map<string, ModelHealthEntry>();
    modelHealth.forEach(h => {
      const ex = bestByModel.get(h.model);
      if (!ex || (h.success && h.latency < ex.latency) || (!ex.success && h.success)) {
        bestByModel.set(h.model, h);
      }
    });
    return aliases.map(a => {
      const h = bestByModel.get(a.target_model);
      return {
        alias: a.alias, target_model: a.target_model, provider: a.provider_id || '',
        success: h ? h.success === 1 : false, latency: h?.latency ?? null, lastChecked: h?.last_checked ?? null,
      };
    });
  }, [aliases, modelHealth]);

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
    return { rate, avg, len: recent.length };
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
        action={
          <>
            <Button
              variant="outline"
              size="sm"
              onClick={loadAll}
            >
              <RefreshCw size={14} className={isLoading ? 'animate-spin text-primary' : 'text-muted-foreground'} />
              <span>{isLoading ? t('dashboard.refreshing') : t('models.actions.refresh')}</span>
            </Button>
            <Button
              size="sm"
              onClick={() => navigate('/accounts')}
            >
              <Plus size={14} />
              <span>{t('dashboard.connectNew')}</span>
            </Button>
          </>
        }
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
        <section className="lg:col-span-7 bg-card border border-border rounded-xl shadow-sm lg:overflow-hidden lg:flex lg:flex-col">
          <div className="p-6 border-b border-border/50">
            <div className="flex justify-between items-center flex-wrap gap-2">
              <div className="flex items-center gap-2">
                <Database size={16} className="text-muted-foreground" />
                <h2 className="text-lg font-bold text-foreground">{t('dashboard.aliasHealth')}</h2>
              </div>
              <Badge variant="secondary" className="bg-success/10 text-success border-success/20 hover:bg-success/20">
                {aliasHealthList.filter(a => a.success).length}/{aliasHealthList.length} OK
              </Badge>
            </div>

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
          </div>

          <div className="divide-y divide-border/50 lg:flex-1 lg:overflow-y-auto">
            {displayAliases.length === 0 ? (
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
        </section>

        {/* Right: Activity Log — 5 cols */}
        <section className="lg:col-span-5 bg-card border border-border rounded-xl shadow-sm lg:overflow-hidden lg:flex lg:flex-col">
          <div className="p-6 border-b border-border/50">
            <div className="flex flex-col sm:flex-row justify-between items-start sm:items-center gap-3">
              <div className="flex items-center gap-2">
                <Activity size={16} className="text-muted-foreground" />
                <h2 className="text-lg font-bold text-foreground">{t('dashboard.recentLogs')}</h2>
              </div>
              <Button
                variant={onlyShowErrors ? "destructive" : "outline"}
                size="sm"
                onClick={() => setOnlyShowErrors(!onlyShowErrors)}
              >
                <AlertTriangle size={12} />
                <span>{onlyShowErrors ? t('dashboard.showAll') : t('dashboard.errorsOnly')}</span>
              </Button>
            </div>

            <div className="grid grid-cols-2 gap-4 mt-6 p-4 bg-muted rounded-xl border border-border/50">
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

            {/* Latency Pulse */}
            <div className="mt-4 space-y-2">
              <div className="flex items-center justify-between">
                <span className="text-[10px] font-bold text-muted-foreground uppercase tracking-widest">{t('dashboard.monitor.latencyPulse')}</span>
                <span className="text-[10px] font-mono text-muted-foreground/60">{activityLogs.length} {t('dashboard.monitor.reqs')}</span>
              </div>
              <div className="h-16 w-full bg-muted/20 rounded-lg p-2 border border-border/40 relative">
                {activityLogs.length > 0 ? (
                  <Bar data={pulseChartData} options={pulseChartOptions} />
                ) : (
                  <div className="h-full flex items-center justify-center text-[10px] text-muted-foreground/30 italic">{t('dashboard.noActivity')}</div>
                )}
              </div>
            </div>
          </div>

          <div className="p-6 space-y-3 lg:flex-1 lg:overflow-y-auto">
            {filteredLogs.length === 0 ? (
              <EmptyState icon={AlertTriangle} title={onlyShowErrors ? t('dashboard.noErrors') : t('dashboard.noActivity')} />
            ) : filteredLogs.map(log => (
              <div key={log.id}
                className={`p-3.5 rounded-xl border text-xs transition-colors duration-150 ${log.success !== 1
                  ? 'bg-destructive/5 border-destructive/10 hover:bg-destructive/10'
                  : 'bg-muted border-border/50 hover:bg-muted/80'}`}
              >
                <div className="flex justify-between items-start gap-2 flex-wrap">
                  <div className="flex items-center gap-2 min-w-0">
                    <span className="text-muted-foreground/60 text-xs shrink-0">
                      {parseServerDate(log.timestamp).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', second: '2-digit', hour12: false })}
                    </span>
                    <span className="font-semibold text-foreground truncate">{log.model}</span>
                  </div>
                  <div className="flex items-center gap-2 shrink-0">
                    <span className="text-muted-foreground text-xs">{log.latency_ms ? `${(log.latency_ms / 1000).toFixed(1)}s` : '--'}</span>
                    <Badge variant={log.success === 1 ? "secondary" : "destructive"} className={log.success === 1 ? 'bg-success/20 text-success border-success/20' : 'bg-destructive/20 text-destructive border-destructive/20'}>
                      {log.success === 1 ? '200' : 'ERR'}
                    </Badge>
                  </div>
                </div>
                {log.account_name && (
                  <div className="mt-1.5 text-xs text-muted-foreground/60">{log.account_name}</div>
                )}
                {log.success !== 1 && log.error_message && (
                  <div className="mt-2 p-2 bg-destructive/10 rounded border border-destructive/20 text-destructive text-xs leading-relaxed">
                    {log.error_message}
                  </div>
                )}
              </div>
            ))}
          </div>

          <div className="p-4 bg-muted border-t border-border/50 flex justify-between items-center text-xs">
            <span className="text-muted-foreground/70">{t('dashboard.logCount', { count: activityLogs.length })}</span>
          </div>
        </section>
      </div>
    </div>
  );
}
