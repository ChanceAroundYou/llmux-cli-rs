import React, { useEffect, useState, useMemo } from 'react';
import { useTranslation } from 'react-i18next';
import { useNavigate } from 'react-router-dom';
import { RefreshCw, Plus, Search, AlertTriangle, Database, Users, Zap, Key, Shield, Activity } from 'lucide-react';
import { parseServerDate } from '../utils/date';

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
        fetch('/api/health'), fetch('/api/models/health'), fetch('/api/activity?limit=30'),
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
    return src.slice(0, 20);
  }, [activityLogs, onlyShowErrors]);

  const logMetrics = useMemo(() => {
    const recent = activityLogs.slice(0, 20);
    const ok = recent.filter(l => l.success === 1).length;
    const rate = recent.length ? Math.round((ok / recent.length) * 100) : 0;
    const avg = recent.length ? Math.round(recent.reduce((a, l) => a + (l.latency_ms || 0), 0) / recent.length) : 0;
    return { rate, avg, len: recent.length };
  }, [activityLogs]);

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
    <div className="space-y-8 animate-in fade-in duration-500">

      {/* Header */}
      <header className="flex flex-col sm:flex-row justify-between items-start sm:items-center gap-4">
        <div>
          <div className="flex items-center gap-2.5">
            <span className="h-2.5 w-2.5 rounded-full bg-blue-600 animate-pulse" />
            <h1 className="text-2xl font-bold tracking-tight text-foreground">{t('common.dashboard')}</h1>
          </div>
          <p className="text-sm text-muted-foreground mt-1">{t('dashboard.subtitle')}</p>
        </div>
        <div className="flex items-center gap-3 w-full sm:w-auto">
          <button
            onClick={loadAll}
            className="flex-1 sm:flex-initial flex items-center justify-center gap-2 px-4 py-2 border border-border bg-card hover:bg-muted active:bg-accent text-foreground/80 text-sm font-semibold rounded-lg shadow-sm transition-all duration-200"
          >
            <RefreshCw size={14} className={isLoading ? 'animate-spin text-primary' : 'text-muted-foreground'} />
            <span>{isLoading ? t('dashboard.refreshing') : t('models.actions.refresh')}</span>
          </button>
          <button
            onClick={() => navigate('/accounts')}
            className="flex-1 sm:flex-initial flex items-center justify-center gap-2 px-4 py-2 bg-blue-600 hover:bg-blue-700 text-white text-sm font-semibold rounded-lg shadow-md hover:shadow-lg active:scale-95 transition-all duration-200"
          >
            <Plus size={14} />
            <span>{t('dashboard.connectNew')}</span>
          </button>
        </div>
      </header>

      {/* 4 Stat Cards */}
      <section className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-4 gap-6">
        <StatCard label={t('dashboard.stats.accounts')} value={accountCount} subtitle={`${t('dashboard.healthy')}: ${healthyCount}`} Icon={Users} color="blue" />
        <StatCard label={t('dashboard.stats.aliases')} value={aliasCount} subtitle={`${t('dashboard.stats.aliasesHint')}: ${aliasCount}`} Icon={Zap} color="amber" />
        <StatCard label={t('dashboard.stats.apiKeys')} value={keyCount} subtitle={t('dashboard.stats.keysHint')} Icon={Key} color="purple" />
        <StatCard label={t('dashboard.stats.healthy')} value={healthyCount} subtitle={`${accountCount > 0 ? Math.round((healthyCount / accountCount) * 100) : 0}% ${t('dashboard.online')}`} Icon={Shield} color="emerald" />
      </section>

      {/* Two-Column Panel */}
      <div className="grid grid-cols-1 lg:grid-cols-12 gap-8 items-start">

        {/* Left: Alias Health — 7 cols */}
        <section className="lg:col-span-7 bg-card border border-border rounded-2xl shadow-sm overflow-hidden flex flex-col">
          <div className="p-6 border-b border-border/50">
            <div className="flex justify-between items-center flex-wrap gap-2">
              <div className="flex items-center gap-2">
                <Database size={16} className="text-muted-foreground" />
                <h2 className="text-lg font-bold text-foreground">{t('dashboard.aliasHealth')}</h2>
              </div>
              <span className="text-xs bg-emerald-50 text-emerald-700 px-2.5 py-1 rounded-full font-semibold border border-emerald-200">
                {aliasHealthList.filter(a => a.success).length}/{aliasHealthList.length} OK
              </span>
            </div>

            <div className="mt-4 space-y-3">
              <div className="relative">
                <Search className="absolute left-3 top-1/2 -translate-y-1/2 text-muted-foreground" size={14} />
                <input
                  type="text" placeholder={t('dashboard.filterAliases')} value={searchQuery}
                  onChange={e => setSearchQuery(e.target.value)}
                  className="block w-full pl-9 pr-4 py-2 border border-border rounded-lg text-sm bg-muted focus:bg-card focus:outline-none focus:ring-2 focus:ring-blue-500/20 focus:border-blue-500 transition-all duration-200"
                />
              </div>
              {providerList.length > 2 && (
                <div className="flex items-center gap-1.5 overflow-x-auto py-1 text-xs">
                  <span className="text-muted-foreground shrink-0 font-medium mr-1">{t('dashboard.providerFilter')}:</span>
                  {providerList.map(prov => (
                    <button key={prov}
                      onClick={() => setSelectedProvider(prov)}
                      className={`px-2.5 py-1 rounded-md font-semibold transition-all duration-150 shrink-0 ${selectedProvider === prov ? 'bg-blue-600 text-white shadow-sm' : 'bg-muted text-muted-foreground hover:bg-accent'}`}
                    >{prov === 'All' ? t('dashboard.all') : prov}</button>
                  ))}
                </div>
              )}
            </div>
          </div>

          <div className="divide-y divide-border/50 max-h-[500px] overflow-y-auto">
            {displayAliases.length === 0 ? (
              <div className="p-12 text-center text-muted-foreground text-sm">{t('dashboard.noAliases')}</div>
            ) : displayAliases.map((a, i) => (
              <div key={i} className="p-5 flex items-center justify-between gap-4 hover:bg-muted/30 transition-colors duration-200">
                <div className="flex items-center gap-4 min-w-0">
                  <div className="relative flex h-3 w-3 shrink-0">
                    {a.success && <span className="animate-ping absolute inline-flex h-full w-full rounded-full bg-emerald-400 opacity-75" />}
                    <span className={`relative inline-flex rounded-full h-3 w-3 ${a.lastChecked ? (a.success ? 'bg-emerald-500' : 'bg-red-500') : 'bg-muted-foreground/30'}`} />
                  </div>
                  <div className="min-w-0">
                    <div className="flex items-center gap-2 flex-wrap">
                      <span className="font-bold text-foreground tracking-tight truncate">{a.alias}</span>
                      {a.provider && (
                        <span className="text-[10px] bg-muted text-muted-foreground font-semibold px-2 py-0.5 rounded border border-border">{a.provider}</span>
                      )}
                    </div>
                    <p className="text-xs text-muted-foreground font-mono mt-1">{a.target_model}</p>
                  </div>
                </div>
                <div className="text-right shrink-0">
                  {a.latency !== null ? (
                    <span className={`text-sm font-bold font-mono px-2 py-1 rounded ${a.success
                      ? a.latency < 500 ? 'text-emerald-600 bg-emerald-50' : a.latency < 1200 ? 'text-amber-600 bg-amber-50' : 'text-blue-600 bg-blue-50'
                      : 'text-red-600 bg-red-50'}`}>
                      {a.success ? `${(a.latency / 1000).toFixed(1)}s` : 'ERR'}
                    </span>
                  ) : a.lastChecked ? (
                    <span className="text-sm font-bold text-red-500 bg-red-50 px-2 py-1 rounded">ERR</span>
                  ) : (
                    <span className="text-[10px] text-muted-foreground/60">{t('dashboard.untested')}</span>
                  )}
                </div>
              </div>
            ))}
          </div>

          <div className="p-4 bg-muted border-t border-border/50 text-[11px] text-muted-foreground flex justify-between">
            <span>{t('dashboard.healthSource')}</span>
            <span>{aliasHealthList.filter(a => a.lastChecked).length} {t('dashboard.tested')}</span>
          </div>
        </section>

        {/* Right: Activity Log — 5 cols */}
        <section className="lg:col-span-5 bg-card border border-border rounded-2xl shadow-sm overflow-hidden flex flex-col">
          <div className="p-6 border-b border-border/50">
            <div className="flex flex-col sm:flex-row justify-between items-start sm:items-center gap-3">
              <div className="flex items-center gap-2">
                <Activity size={16} className="text-blue-600" />
                <h2 className="text-lg font-bold text-foreground">{t('dashboard.recentLogs')}</h2>
              </div>
              <button
                onClick={() => setOnlyShowErrors(!onlyShowErrors)}
                className={`flex items-center gap-1.5 px-3 py-1 rounded-md text-xs font-bold transition-all border ${onlyShowErrors
                  ? 'bg-rose-50 border-rose-200 text-rose-700 hover:bg-rose-100'
                  : 'bg-muted border-border text-muted-foreground hover:bg-accent'}`}
              >
                <AlertTriangle size={12} />
                <span>{onlyShowErrors ? t('dashboard.showAll') : t('dashboard.errorsOnly')}</span>
              </button>
            </div>

            <div className="grid grid-cols-2 gap-4 mt-6 p-4 bg-muted rounded-xl border border-border/50">
              <div>
                <span className="text-[11px] font-medium text-muted-foreground block uppercase tracking-wider">{t('dashboard.monitor.successRate')}</span>
                <div className="flex items-baseline gap-1 mt-1">
                  <span className="text-xl font-black text-foreground tracking-tight">{logMetrics.rate}%</span>
                </div>
              </div>
              <div>
                <span className="text-[11px] font-medium text-muted-foreground block uppercase tracking-wider">{t('dashboard.monitor.avgLag')}</span>
                <div className="flex items-baseline gap-1 mt-1">
                  <span className="text-xl font-black text-foreground tracking-tight">{(logMetrics.avg / 1000).toFixed(1)}s</span>
                </div>
              </div>
            </div>
          </div>

          <div className="flex-1 p-6 max-h-[420px] overflow-y-auto min-h-[280px] space-y-3">
            {filteredLogs.length === 0 ? (
              <div className="flex flex-col items-center justify-center py-20 text-center text-muted-foreground/40">
                <AlertTriangle size={40} className="mb-2 opacity-30" />
                <p className="text-sm">{onlyShowErrors ? t('dashboard.noErrors') : t('dashboard.noActivity')}</p>
              </div>
            ) : filteredLogs.map(log => (
              <div key={log.id}
                className={`p-3.5 rounded-xl border text-xs transition-all duration-300 ${log.success !== 1
                  ? 'bg-rose-50/50 border-rose-100 hover:bg-rose-50'
                  : 'bg-muted border-border/50 hover:bg-muted/80'}`}
              >
                <div className="flex justify-between items-start gap-2 flex-wrap">
                  <div className="flex items-center gap-2 min-w-0">
                    <span className="text-muted-foreground/60 text-[10px] shrink-0">
                      {parseServerDate(log.timestamp).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', second: '2-digit', hour12: false })}
                    </span>
                    <span className="font-bold text-foreground truncate">{log.model}</span>
                  </div>
                  <div className="flex items-center gap-2 shrink-0">
                    <span className="text-muted-foreground text-[11px]">{log.latency_ms ? `${(log.latency_ms / 1000).toFixed(1)}s` : '--'}</span>
                    <span className={`px-1.5 py-0.5 rounded text-[10px] font-extrabold ${log.success === 1 ? 'bg-emerald-100 text-emerald-800' : 'bg-rose-100 text-rose-800'}`}>
                      {log.success === 1 ? '200' : 'ERR'}
                    </span>
                  </div>
                </div>
                {log.account_name && (
                  <div className="mt-1.5 text-[10px] text-muted-foreground/60">{log.account_name}</div>
                )}
                {log.success !== 1 && log.error_message && (
                  <div className="mt-2 p-2 bg-rose-100/40 rounded border border-rose-200/30 text-rose-700 text-[10px] leading-relaxed">
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

function StatCard({ label, value, subtitle, Icon, color }: {
  label: string; value: number; subtitle: string; Icon: React.ComponentType<any>; color: string;
}) {
  const colors: Record<string, { bg: string; iconBg: string; iconColor: string; text: string }> = {
    blue:    { bg: 'bg-blue-50',    iconBg: 'bg-blue-100',    iconColor: 'text-blue-600',    text: 'text-blue-600' },
    amber:   { bg: 'bg-amber-50',   iconBg: 'bg-amber-100',   iconColor: 'text-amber-600',   text: 'text-amber-600' },
    purple:  { bg: 'bg-purple-50',  iconBg: 'bg-purple-100',  iconColor: 'text-purple-600',  text: 'text-purple-600' },
    emerald: { bg: 'bg-emerald-50', iconBg: 'bg-emerald-100', iconColor: 'text-emerald-600', text: 'text-emerald-600' },
  };
  const c = colors[color] || colors.blue;
  return (
    <div className="bg-card border border-border p-5 rounded-2xl shadow-sm hover:shadow-md transition-shadow duration-300 relative overflow-hidden group">
      <div className={`absolute top-0 right-0 h-16 w-16 ${c.bg} rounded-bl-full group-hover:scale-110 transition-transform duration-300`} />
      <div className="relative flex justify-between items-start">
        <div>
          <p className="text-sm font-medium text-muted-foreground">{label}</p>
          <h3 className="text-3xl font-extrabold text-foreground mt-2 tracking-tight">{value}</h3>
        </div>
        <div className={`p-3 ${c.iconBg} rounded-xl`}>
          <Icon size={20} className={c.iconColor} />
        </div>
      </div>
      <div className={`mt-4 flex items-center text-xs ${c.text} font-semibold gap-1`}>
        <span>{subtitle}</span>
      </div>
    </div>
  );
}
