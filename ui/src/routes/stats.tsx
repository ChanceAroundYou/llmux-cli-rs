import React, { useEffect, useState, useMemo, useCallback } from 'react';
import { apiFetch } from "@/lib/api";
import { useTranslation } from 'react-i18next';
import { BarChart3, RefreshCw, Search, Inbox } from 'lucide-react';
import { fmtSec, fmtTokens } from '../utils/format';
import { PageHeader } from '../components/shared/PageHeader';
import { StatCard } from '../components/shared/StatCard';
import { EmptyState } from '../components/shared/EmptyState';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { Badge } from '@/components/ui/badge';
import {
  Chart as ChartJS,
  CategoryScale,
  LinearScale,
  BarElement,
  LineElement,
  PointElement,
  Tooltip,
  Legend,
  Filler,
  type ChartData,
  type ChartOptions,
} from 'chart.js';
import { Bar, Line } from 'react-chartjs-2';

ChartJS.register(CategoryScale, LinearScale, BarElement, LineElement, PointElement, Tooltip, Legend, Filler);

type Preset = '1h' | '24h' | '7d' | '30d' | 'custom';

interface Summary {
  total_input: number;
  total_output: number;
  total_cache_read: number;
  total_cache_create: number;
  cache_hit_rate: number;
  avg_latency: number;
  p95_latency: number;
  avg_ttft: number;
  p95_ttft: number;
  avg_tps: number;
  total_requests: number;
  success_requests: number;
}
interface ModelBreakdown {
  model: string | null;
  input: number;
  output: number;
  cacheRead: number;
  cacheCreate: number;
  cacheHitRate: number;
  requests: number;
  successCount: number;
  avgLatency: number;
  avgTtft: number;
  p95Ttft: number;
  avgTps: number;
}
interface AccountBreakdown {
  id: number;
  name: string;
  provider: string;
  input: number;
  output: number;
  cacheRead: number;
  cacheCreate: number;
  cacheHitRate: number;
  totalTokens: number;
  requests: number;
  successCount: number;
  avgLatency: number;
  avgTtft: number;
  p95Ttft: number;
  avgTps: number;
}

const PRESET_MS: Record<Exclude<Preset, 'custom'>, number> = {
  '1h': 60 * 60 * 1000,
  '24h': 24 * 60 * 60 * 1000,
  '7d': 7 * 24 * 60 * 60 * 1000,
  '30d': 30 * 24 * 60 * 60 * 1000,
};

function toLocalInput(ms: number): string {
  const d = new Date(ms);
  const pad = (n: number) => String(n).padStart(2, '0');
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`;
}
function fromLocalInput(v: string): number {
  return new Date(v).getTime();
}

export default function StatsPage() {
  const { t } = useTranslation();
  const [preset, setPreset] = useState<Preset>('24h');
  const [customStart, setCustomStart] = useState(() => toLocalInput(Date.now() - PRESET_MS['24h']));
  const [customEnd, setCustomEnd] = useState(() => toLocalInput(Date.now()));
  const [range, setRange] = useState(() => {
    const end = Date.now();
    return { start: end - PRESET_MS['24h'], end };
  });

  const [summary, setSummary] = useState<Summary | null>(null);
  const [byModel, setByModel] = useState<ModelBreakdown[]>([]);
  const [byAccount, setByAccount] = useState<AccountBreakdown[]>([]);
  const [timeseries, setTimeseries] = useState<Array<{ bucket: number; input: number; output: number; cacheRead: number; cacheCreate: number; requests: number; avgLatency: number; p95Latency: number; avgTtft: number; p95Ttft: number; avgTps: number }>>([]);
  const [granularityMs, setGranularityMs] = useState<number | null>(null);
  const [loading, setLoading] = useState(false);
  const [modelFilter, setModelFilter] = useState('');
  const [accountFilter, setAccountFilter] = useState('');
  const [modelSort, setModelSort] = useState<{ key: string; dir: 'asc' | 'desc' }>({ key: 'requests', dir: 'desc' });
  const [accountSort, setAccountSort] = useState<{ key: string; dir: 'asc' | 'desc' }>({ key: 'requests', dir: 'desc' });

  const applyPreset = (p: Exclude<Preset, 'custom'>) => {
    setPreset(p);
    const end = Date.now();
    setRange({ start: end - PRESET_MS[p], end });
  };
  const applyCustom = () => {
    const s = fromLocalInput(customStart);
    const e = fromLocalInput(customEnd);
    if (!isNaN(s) && !isNaN(e) && s <= e) {
      setPreset('custom');
      setRange({ start: s, end: e });
    }
  };

  const fetchStats = useCallback(async () => {
    setLoading(true);
    try {
      const res = await apiFetch(`/api/stats?start=${range.start}&end=${range.end}`);
      if (!res.ok) throw new Error(await res.text());
      const data = await res.json();
      setSummary(data.summary ?? null);
      setByModel(data.byModel ?? []);
      setByAccount(data.byAccount ?? []);
      setTimeseries(Array.isArray(data.timeseries) ? data.timeseries : []);
      setGranularityMs(typeof data.granularityMs === 'number' ? data.granularityMs : null);
    } catch (e) {
      console.error('stats fetch failed', e);
    } finally {
      setLoading(false);
    }
  }, [range.start, range.end]);

  useEffect(() => { fetchStats(); }, [fetchStats]);

  const filteredModels = useMemo(() => {
    if (!modelFilter) return byModel;
    const q = modelFilter.toLowerCase();
    return byModel.filter(m => (m.model ?? '').toLowerCase().includes(q));
  }, [byModel, modelFilter]);
  const filteredAccounts = useMemo(() => {
    if (!accountFilter) return byAccount;
    const q = accountFilter.toLowerCase();
    return byAccount.filter(a => a.name.toLowerCase().includes(q) || a.provider.toLowerCase().includes(q));
  }, [byAccount, accountFilter]);

  const toggleModelSort = (k: string) => setModelSort(prev => prev.key === k ? { key: k, dir: prev.dir === 'asc' ? 'desc' : 'asc' } : { key: k, dir: 'desc' });
  const toggleAccountSort = (k: string) => setAccountSort(prev => prev.key === k ? { key: k, dir: prev.dir === 'asc' ? 'desc' : 'asc' } : { key: k, dir: 'desc' });
  const sortIndicator = (cur: {key:string, dir:string}, k: string) => cur.key !== k ? ' ↕' : cur.dir === 'asc' ? ' ▲' : ' ▼';

  const sortedModels = useMemo(() => {
    const arr = [...filteredModels];
    const dir = modelSort.dir === 'asc' ? 1 : -1;
    const getVal = (m: any) => {
      switch(modelSort.key) {
        case 'model': return (m.model ?? '').toLowerCase();
        case 'input': return m.input;
        case 'output': return m.output;
        case 'cacheHit': return m.cacheRead;
        case 'hitRate': return m.cacheHitRate;
        case 'requests': return m.requests;
        case 'successRate': return m.requests ? m.successCount / m.requests : 0;
        case 'avgLatency': return m.avgLatency;
        case 'avgTtft': return m.avgTtft;
        case 'avgTps': return m.avgTps;
        default: return 0;
      }
    };
    arr.sort((a,b) => {
      const av=getVal(a), bv=getVal(b);
      if (typeof av === 'string') return (av as string).localeCompare(bv as string) * dir;
      return ((av as number) - (bv as number)) * dir;
    });
    return arr;
  }, [filteredModels, modelSort]);

  const sortedAccounts = useMemo(() => {
    const arr = [...filteredAccounts];
    const dir = accountSort.dir === 'asc' ? 1 : -1;
    const getVal = (a: any) => {
      switch(accountSort.key) {
        case 'name': return (a.name ?? '').toLowerCase();
        case 'input': return a.input;
        case 'output': return a.output;
        case 'cacheHit': return a.cacheRead;
        case 'hitRate': return a.cacheHitRate;
        case 'requests': return a.requests;
        case 'successRate': return a.requests ? a.successCount / a.requests : 0;
        case 'avgLatency': return a.avgLatency;
        case 'avgTtft': return a.avgTtft;
        case 'avgTps': return a.avgTps;
        default: return 0;
      }
    };
    arr.sort((a,b) => {
      const av=getVal(a), bv=getVal(b);
      if (typeof av === 'string') return (av as string).localeCompare(bv as string) * dir;
      return ((av as number) - (bv as number)) * dir;
    });
    return arr;
  }, [filteredAccounts, accountSort]);

  // 6 色系循环，每系内 3 阶：缓存(深, 底部) → 输入(中) → 输出(浅, 顶部)
  const HUE_FAMILIES: Array<{ cache: string; input: string; output: string }> = [
    { cache: '#1d4ed8', input: '#3b82f6', output: '#93c5fd' }, // blue
    { cache: '#15803d', input: '#22c55e', output: '#86efac' }, // green
    { cache: '#6d28d9', input: '#8b5cf6', output: '#c4b5fd' }, // violet
    { cache: '#b45309', input: '#f59e0b', output: '#fde68a' }, // amber
    { cache: '#0e7490', input: '#06b6d4', output: '#67e8f9' }, // cyan
    { cache: '#be123c', input: '#ec4899', output: '#f9a8d4' }, // rose
  ];

  const modelChartData: ChartData<'bar'> = useMemo(() => {
    const top = [...byModel].slice(0, 8);
    return {
      labels: top.map(m => (m.model ?? 'unknown').slice(0, 28)),
      datasets: [
        { label: t('usage.legend.cache', { defaultValue: '缓存' }), data: top.map(m => (m.cacheRead ?? 0)), backgroundColor: top.map((_, i) => HUE_FAMILIES[i % HUE_FAMILIES.length].cache), stack: 'tokens' },
        { label: t('usage.legend.input', { defaultValue: '输入' }), data: top.map(m => m.input), backgroundColor: top.map((_, i) => HUE_FAMILIES[i % HUE_FAMILIES.length].input), stack: 'tokens' },
        { label: t('usage.legend.output', { defaultValue: '输出' }), data: top.map(m => m.output), backgroundColor: top.map((_, i) => HUE_FAMILIES[i % HUE_FAMILIES.length].output), stack: 'tokens' },
      ],
    };
  }, [byModel, t]);

  const accountChartData: ChartData<'bar'> = useMemo(() => {
    const top = [...byAccount].slice(0, 8);
    return {
      labels: top.map(a => a.name.slice(0, 20)),
      datasets: [
        { label: t('usage.legend.cache', { defaultValue: '缓存' }), data: top.map(a => (a.cacheRead ?? 0)), backgroundColor: top.map((_, i) => HUE_FAMILIES[i % HUE_FAMILIES.length].cache), stack: 'tokens' },
        { label: t('usage.legend.input', { defaultValue: '输入' }), data: top.map(a => a.input), backgroundColor: top.map((_, i) => HUE_FAMILIES[i % HUE_FAMILIES.length].input), stack: 'tokens' },
        { label: t('usage.legend.output', { defaultValue: '输出' }), data: top.map(a => a.output), backgroundColor: top.map((_, i) => HUE_FAMILIES[i % HUE_FAMILIES.length].output), stack: 'tokens' },
      ],
    };
  }, [byAccount, t]);

  const barOpts: ChartOptions<'bar'> = useMemo(() => ({
    responsive: true, maintainAspectRatio: false, animation: false,
    interaction: { mode: 'index', intersect: false } as const,
    plugins: {
      legend: { display: true, position: 'bottom' as const, labels: { boxWidth: 10, font: { size: 10 }, padding: 12 } },
      tooltip: { backgroundColor: '#1e293b', titleFont: { size: 10 }, bodyFont: { size: 10 }, itemSort: (a: any, b: any) => b.datasetIndex - a.datasetIndex /* stack bottom->top: cache(0)->input(1)->output(2), so top->down = output(2), input(1), cache(0) */ },
    },
    scales: { x: { stacked: true, ticks: { maxRotation: 0, font: { size: 9 } } }, y: { stacked: true, beginAtZero: true } },
  }), []);

  const tokenTimeseriesData: ChartData<'line'> = useMemo(() => {
    const labels = timeseries.map(p => {
      const d = new Date(p.bucket);
      const span = range.end - range.start;
      if (span <= 2 * 60 * 60 * 1000) return d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
      if (span <= 48 * 60 * 60 * 1000) return d.toLocaleString([], { month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit' });
      return d.toLocaleDateString([], { month: '2-digit', day: '2-digit' });
    });
    return {
      labels,
      datasets: [
        { label: t('usage.legend.cache', { defaultValue: '缓存' }), data: timeseries.map(p => (p.cacheRead ?? 0)), borderColor: '#1d4ed8', backgroundColor: 'transparent', fill: false, tension: 0.28, pointRadius: 0, borderWidth: 1.6 },
        { label: t('usage.legend.input', { defaultValue: '输入' }), data: timeseries.map(p => p.input), borderColor: '#3b82f6', backgroundColor: 'transparent', fill: false, tension: 0.28, pointRadius: 0, borderWidth: 1.6 },
        { label: t('usage.legend.output', { defaultValue: '输出' }), data: timeseries.map(p => p.output), borderColor: '#22c55e', backgroundColor: 'transparent', fill: false, tension: 0.28, pointRadius: 0, borderWidth: 1.6 },
      ],
    };
  }, [timeseries, range.start, range.end, t]);

  const tokenTimeseriesOpts: ChartOptions<'line'> = useMemo(() => ({
    responsive: true, maintainAspectRatio: false, animation: false,
    interaction: { mode: 'index', intersect: false } as const,
    plugins: {
      legend: { display: true, position: 'bottom' as const, labels: { boxWidth: 10, font: { size: 10 }, padding: 12 } },
      tooltip: { backgroundColor: '#1e293b', titleFont: { size: 10 }, bodyFont: { size: 10 }, itemSort: (a: any, b: any) => b.datasetIndex - a.datasetIndex },
    },
    scales: {
      x: { stacked: true, ticks: { maxRotation: 0, font: { size: 9 }, maxTicksLimit: 12 } },
      y: { stacked: true, beginAtZero: true, ticks: { font: { size: 9 } } },
    },
  }), []);

  const tsLabels: string[] = useMemo(() => timeseries.map(p => {
    const d = new Date(p.bucket);
    const span = range.end - range.start;
    if (span <= 2 * 60 * 60 * 1000) return d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
    if (span <= 48 * 60 * 60 * 1000) return d.toLocaleString([], { month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit' });
    return d.toLocaleDateString([], { month: '2-digit', day: '2-digit' });
  }), [timeseries, range.start, range.end]);

  // Dual-axis: 净耗时 (avg/p95, amber) + TTFT (avg/p95, green) — 同色实线 avg / 虚线 p95，图例 2x2 同色上下对齐
  const latencyDualData: ChartData<'line'> = useMemo(() => ({
    labels: tsLabels,
    datasets: [
      { label: t('usage.latency.avg', { defaultValue: '平均耗时' }), data: timeseries.map(p => Math.max(0, Math.round(p.avgLatency - p.avgTtft)) / 1000), borderColor: '#f59e0b', backgroundColor: 'transparent', fill: false, tension: 0.28, pointRadius: 0, borderWidth: 1.8, yAxisID: 'y' },
      { label: t('usage.ttft.avg', { defaultValue: '平均 TTFT' }), data: timeseries.map(p => Math.round(p.avgTtft) / 1000), borderColor: '#22c55e', backgroundColor: 'transparent', fill: false, tension: 0.28, pointRadius: 0, borderWidth: 1.8, yAxisID: 'y' },
      { label: t('usage.latency.p95', { defaultValue: 'P95 耗时' }), data: timeseries.map(p => Math.max(0, Math.round(p.p95Latency - p.p95Ttft)) / 1000), borderColor: '#f59e0b', backgroundColor: 'transparent', fill: false, tension: 0.28, pointRadius: 0, borderWidth: 1.6, borderDash: [6, 4], yAxisID: 'y' },
      { label: t('usage.ttft.p95', { defaultValue: 'P95 TTFT' }), data: timeseries.map(p => Math.round(p.p95Ttft) / 1000), borderColor: '#22c55e', backgroundColor: 'transparent', fill: false, tension: 0.28, pointRadius: 0, borderWidth: 1.6, borderDash: [6, 4], yAxisID: 'y' },
    ],
  }), [timeseries, tsLabels, t]);

  const latencyDualOpts: ChartOptions<'line'> = useMemo(() => ({
    responsive: true, maintainAspectRatio: false, animation: false,
    interaction: { mode: 'index', intersect: false } as const,
    plugins: {
      legend: {
        display: true, position: 'bottom' as const,
        maxWidth: 520,
        labels: {
          boxWidth: 14, boxHeight: 2, usePointStyle: false, font: { size: 10 }, padding: 12,
          // 让同色系上下对齐：图例按 2 列排，顺序为 [黄实线, 绿实线, 黄虚线, 绿虚线] → 视觉上 2x2 同色上下对齐
          generateLabels: (chart: any) => {
            const ds = chart.data.datasets as any[];
            const base = (ChartJS.defaults as any).plugins.legend.labels.generateLabels(chart) as any[];
            // base 顺序与 datasets 一致：0:黄实线(avg耗时) 1:绿实线(avg TTFT) 2:黄虚线(p95耗时) 3:绿虚线(p95 TTFT)
            // 重排为 2x2：第一行同色实线，第二行同色虚线，利用 legend 的多行自动换行+固定 maxWidth 形成 2 列
            // 保持颜色与线型一致，仅调整显示顺序为 [0,1,2,3] 时天然已是 2 行时每行同色上下对齐（Chart.js 按 maxWidth 换行）
            return base.map((lb: any, i: number) => {
              const d = ds[i];
              lb.fillStyle = d.borderColor as string;
              lb.strokeStyle = d.borderColor as string;
              lb.lineWidth = 2;
              lb.lineDash = (d.borderDash as number[] | undefined) ?? [];
              return lb;
            });
          },
        },
      },
      tooltip: { backgroundColor: '#1e293b', titleFont: { size: 10 }, bodyFont: { size: 10 }, callbacks: { label: (c: any) => ` ${c.dataset.label}: ${Number(c.parsed.y).toFixed(1)}s` } },
    },
    scales: {
      x: { stacked: false, ticks: { maxRotation: 0, font: { size: 9 }, maxTicksLimit: 12 } },
      y: { type: 'linear' as const, position: 'left' as const, beginAtZero: true, title: { display: true, text: String(t('usage.charts.netElapsed', { defaultValue: '净耗时 (s)' })), color: '#f59e0b', font: { size: 9 } }, ticks: { color: '#f59e0b', font: { size: 9 }, callback: (v: any) => `${Number(v).toFixed(1)}s` } },
      y1: { type: 'linear' as const, position: 'right' as const, beginAtZero: true, title: { display: true, text: String(t('usage.charts.ttftAxis', { defaultValue: 'TTFT (s)' })), color: '#22c55e', font: { size: 9 } }, ticks: { color: '#22c55e', font: { size: 9 }, callback: (v: any) => `${Number(v).toFixed(1)}s` }, grid: { drawOnChartArea: false } },
    },
  }), []);

  const throughputData: ChartData<'line'> = useMemo(() => ({
    labels: tsLabels,
    datasets: [
      { label: 'Token/s', data: timeseries.map(p => Math.round(p.avgTps * 10) / 10), borderColor: '#22c55e', backgroundColor: 'transparent', fill: false, tension: 0.28, pointRadius: 0, borderWidth: 1.6 },
    ],
  }), [timeseries, tsLabels, t]);

  const throughputOpts: ChartOptions<'line'> = useMemo(() => ({
    responsive: true, maintainAspectRatio: false, animation: false,
    interaction: { mode: 'index', intersect: false } as const,
    plugins: {
      legend: { display: true, position: 'bottom' as const, labels: { boxWidth: 10, font: { size: 10 }, padding: 12 } },
      tooltip: { backgroundColor: '#1e293b', titleFont: { size: 10 }, bodyFont: { size: 10 } },
    },
    scales: {
      x: { stacked: false, ticks: { maxRotation: 0, font: { size: 9 }, maxTicksLimit: 12 } },
      y: { stacked: false, beginAtZero: true, ticks: { font: { size: 9 }, callback: (v: any) => `${Number(v).toFixed(1)} Token/s` } },
    },
  }), []);

  const totalTokens = (summary?.total_input ?? 0) + (summary?.total_output ?? 0) + (summary?.total_cache_read ?? 0) + (summary?.total_cache_create ?? 0);
  const totalCache = (summary?.total_cache_read ?? 0); // 4-store-3-display: cache = read, creation hidden
  const successRate = summary?.total_requests ? Math.round((summary.success_requests / summary.total_requests) * 100) : 0;
  const avgNetLatency = Math.max(0, (summary?.avg_latency ?? 0) - (summary?.avg_ttft ?? 0));
  const p95NetLatency = Math.max(0, (summary?.p95_latency ?? 0) - (summary?.p95_ttft ?? 0));

  return (
    <div className="flex flex-col gap-6 animate-fadeIn pb-8">
      <PageHeader
        icon={<BarChart3 size={24} />}
        title={t('common.usage', { defaultValue: '用量统计' })}
        subtitle={t('usage.subtitle', { defaultValue: '按时间与维度查看 token 消耗' })}
        action={
          <Button variant="outline" size="sm" onClick={fetchStats} disabled={loading}>
            <RefreshCw size={14} className={loading ? 'animate-spin' : ''} />
            <span>{loading ? '...' : t('models.actions.refresh', { defaultValue: '刷新' })}</span>
          </Button>
        }
      />

      {/* Time range */}
      <div className="bg-card border border-border rounded-xl p-4 flex flex-col gap-3">
        <div className="flex flex-wrap gap-2">
          {(['1h', '24h', '7d', '30d'] as const).map(p => (
            <Button key={p} variant={preset === p ? 'default' : 'outline'} size="sm" onClick={() => applyPreset(p)} className="h-7 px-3 text-xs">
              {t(`usage.presets.${p}`, { defaultValue: p === '1h' ? '近 1 小时' : p === '24h' ? '近 24 小时' : p === '7d' ? '近 7 天' : '近 30 天' })}
            </Button>
          ))}
        </div>
        <div className="flex flex-wrap items-end gap-2">
          <div className="flex flex-col gap-1">
            <span className="text-xs text-muted-foreground">{t('usage.custom.start', { defaultValue: '开始' })}</span>
            <Input type="datetime-local" value={customStart} onChange={e => setCustomStart(e.target.value)} className="h-8 text-xs" />
          </div>
          <div className="flex flex-col gap-1">
            <span className="text-xs text-muted-foreground">{t('usage.custom.end', { defaultValue: '结束' })}</span>
            <Input type="datetime-local" value={customEnd} onChange={e => setCustomEnd(e.target.value)} className="h-8 text-xs" />
          </div>
          <Button size="sm" variant={preset === 'custom' ? 'default' : 'outline'} onClick={applyCustom} className="h-8">{t('usage.custom.apply', { defaultValue: '应用' })}</Button>
          <span className="text-xs text-muted-foreground ml-2">
            {new Date(range.start).toLocaleString()} — {new Date(range.end).toLocaleString()}
          </span>
        </div>
      </div>

      {/* Summary */}
      <section className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-3 gap-4">
        <StatCard label={t('usage.statsCards.totalTokens', { defaultValue: '总 Token' })} value={totalTokens.toLocaleString()} subtitle={`${t('usage.output', { defaultValue: '输出' })} ${fmtTokens(summary?.total_output)} · ${t('usage.input', { defaultValue: '输入' })} ${fmtTokens(summary?.total_input)} · ${t('usage.cacheHit', { defaultValue: '缓存' })} ${fmtTokens(totalCache)}`} icon={BarChart3} />
        <StatCard label={t('usage.statsCards.cacheHit', { defaultValue: '缓存命中' })} value={`${(summary?.cache_hit_rate ?? 0).toFixed(1)}%`} icon={BarChart3} />
        <StatCard label={t('usage.statsCards.requests', { defaultValue: '请求' })} value={summary?.total_requests ?? 0} subtitle={`${t('usage.success', { defaultValue: '成功' })} ${successRate}%`} icon={BarChart3} />
        <StatCard label={t('usage.statsCards.avgLatency', { defaultValue: '平均耗时' })} value={fmtSec(avgNetLatency)} subtitle={`P95 ${fmtSec(p95NetLatency)}`} icon={BarChart3} />
        <StatCard label={t('usage.statsCards.avgTtft', { defaultValue: '平均 TTFT' })} value={fmtSec(summary?.avg_ttft ?? 0)} subtitle={`P95 ${fmtSec(summary?.p95_ttft ?? 0)}`} icon={BarChart3} />
        <StatCard label={t('usage.statsCards.avgTps', { defaultValue: '平均 Token/s' })} value={`${(summary?.avg_tps ?? 0).toFixed(1)} Token/s`} icon={BarChart3} />
      </section>

      {/* Charts */}
      <div className="bg-card border border-border rounded-xl p-4">
        <h3 className="text-sm font-bold mb-3">{t('usage.charts.token', { defaultValue: 'Token' })}</h3>
        <div className="h-64">
          {timeseries.length ? <Line data={tokenTimeseriesData} options={tokenTimeseriesOpts} /> : <EmptyState icon={Inbox} title={t('usage.noData', { defaultValue: '暂无数据' })} />}
        </div>
      </div>

      {/* 耗时 & TTFT — 净耗时(黄) + TTFT(绿)，同色实线 avg / 虚线 P95，图例 2x2 同色上下对齐 */}
      <div className="bg-card border border-border rounded-xl p-4">
        <h3 className="text-sm font-bold mb-3">{t('usage.charts.latencyDual', { defaultValue: '耗时 & TTFT' })}</h3>
        <div className="h-64">
          {timeseries.length ? <Line data={latencyDualData} options={latencyDualOpts} /> : <EmptyState icon={Inbox} title={t('usage.noData', { defaultValue: '暂无数据' })} />}
        </div>
      </div>

      {/* Token/s */}
      <div className="bg-card border border-border rounded-xl p-4">
        <h3 className="text-sm font-bold mb-3">{t('usage.charts.throughput', { defaultValue: 'Token/s' })}</h3>
        <div className="h-64">
          {timeseries.length ? <Line data={throughputData} options={throughputOpts} /> : <EmptyState icon={Inbox} title={t('usage.noData', { defaultValue: '暂无数据' })} />}
        </div>
      </div>

      <div className="grid grid-cols-1 lg:grid-cols-2 gap-6">
        <div className="bg-card border border-border rounded-xl p-4">
          <h3 className="text-sm font-bold mb-3">{t('usage.charts.byModelTop', { defaultValue: '按模型 Top' })}</h3>
          <div className="h-64">
            {byModel.length ? <Bar data={modelChartData} options={barOpts} /> : <EmptyState icon={Inbox} title={t('usage.noData', { defaultValue: '暂无数据' })} />}
          </div>
        </div>
        <div className="bg-card border border-border rounded-xl p-4">
          <h3 className="text-sm font-bold mb-3">{t('usage.charts.byAccountTop', { defaultValue: '按账号 Top' })}</h3>
          <div className="h-64">
            {byAccount.length ? <Bar data={accountChartData} options={barOpts} /> : <EmptyState icon={Inbox} title={t('usage.noData', { defaultValue: '暂无数据' })} />}
          </div>
        </div>
      </div>

      {/* By model table */}
      <div className="bg-card border border-border rounded-xl overflow-hidden">
        <div className="p-4 border-b border-border/50 flex flex-wrap justify-between gap-3">
          <h3 className="text-sm font-bold">{t('usage.tables.byModel', { defaultValue: '按模型' })}</h3>
          <div className="relative">
            <Search size={14} className="absolute left-2.5 top-1/2 -translate-y-1/2 text-muted-foreground" />
            <Input placeholder={t('usage.filter.model', { defaultValue: '筛选模型' })} value={modelFilter} onChange={e => setModelFilter(e.target.value)} className="h-8 pl-8 w-56 text-xs" />
          </div>
        </div>
        <div className="overflow-x-auto">
          <table className="w-full text-xs">
            <thead className="bg-muted/50 text-muted-foreground">
              <tr>
                <th className="text-left px-4 py-2 cursor-pointer select-none" onClick={() => toggleModelSort('model')}>{t('usage.tables.headers.model', { defaultValue: '模型' })}{sortIndicator(modelSort,'model')}</th>
                <th className="text-right px-3 py-2 cursor-pointer select-none" onClick={() => toggleModelSort('input')}>{t('usage.input', { defaultValue: '输入' })}{sortIndicator(modelSort,'input')}</th>
                <th className="text-right px-3 py-2 cursor-pointer select-none" onClick={() => toggleModelSort('output')}>{t('usage.output', { defaultValue: '输出' })}{sortIndicator(modelSort,'output')}</th>
                <th className="text-right px-3 py-2 cursor-pointer select-none" onClick={() => toggleModelSort('cacheHit')}>{t('usage.cacheHit', { defaultValue: '缓存' })}{sortIndicator(modelSort,'cacheHit')}</th>
                <th className="text-right px-3 py-2 cursor-pointer select-none" onClick={() => toggleModelSort('hitRate')}>{t('usage.tables.headers.hitRate', { defaultValue: '命中率' })}{sortIndicator(modelSort,'hitRate')}</th>
                <th className="text-right px-3 py-2 cursor-pointer select-none" onClick={() => toggleModelSort('requests')}>{t('usage.tables.headers.requests', { defaultValue: '请求' })}{sortIndicator(modelSort,'requests')}</th>
                <th className="text-right px-3 py-2 cursor-pointer select-none" onClick={() => toggleModelSort('successRate')}>{t('usage.tables.headers.successRate', { defaultValue: '成功率' })}{sortIndicator(modelSort,'successRate')}</th>
                <th className="text-right px-3 py-2 cursor-pointer select-none" onClick={() => toggleModelSort('avgLatency')}>{t('usage.tables.headers.avgLatency', { defaultValue: '平均耗时' })}{sortIndicator(modelSort,'avgLatency')}</th>
                <th className="text-right px-3 py-2 cursor-pointer select-none" onClick={() => toggleModelSort('avgTtft')}>{t('usage.tables.headers.avgTtft', { defaultValue: '平均 TTFT' })}{sortIndicator(modelSort,'avgTtft')}</th>
                <th className="text-right px-3 py-2 cursor-pointer select-none" onClick={() => toggleModelSort('avgTps')}>{t('usage.tables.headers.avgTps', { defaultValue: 'Token/s' })}{sortIndicator(modelSort,'avgTps')}</th>
              </tr>
            </thead>
            <tbody className="divide-y divide-border/50">
              {sortedModels.length === 0 ? (
                <tr><td colSpan={10} className="py-8"><EmptyState icon={Inbox} title={t('usage.noData', { defaultValue: '暂无数据' })} /></td></tr>
              ) : sortedModels.map((m, i) => (
                <tr key={i} className="hover:bg-muted/30">
                  <td className="px-4 py-2 font-mono truncate max-w-[260px]">{m.model ?? '—'}</td>
                  <td className="text-right px-3 py-2">{fmtTokens(m.input)}</td>
                  <td className="text-right px-3 py-2">{fmtTokens(m.output)}</td>
                  <td className="text-right px-3 py-2">{fmtTokens(m.cacheRead ?? 0)}</td>
                  <td className="text-right px-3 py-2">{m.cacheHitRate.toFixed(1)}%</td>
                  <td className="text-right px-3 py-2">{m.requests}</td>
                  <td className="text-right px-3 py-2">{m.requests ? Math.round(m.successCount / m.requests * 100) : 0}%</td>
                  <td className="text-right px-3 py-2">{fmtSec(Math.max(0, m.avgLatency - m.avgTtft))}</td>
                  <td className="text-right px-3 py-2">{fmtSec(m.avgTtft)}</td>
                  <td className="text-right px-3 py-2">{m.avgTps.toFixed(1)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      </div>

      {/* By account table */}
      <div className="bg-card border border-border rounded-xl overflow-hidden">
        <div className="p-4 border-b border-border/50 flex flex-wrap justify-between gap-3">
          <h3 className="text-sm font-bold">{t('usage.tables.byAccount', { defaultValue: '按账号' })}</h3>
          <div className="relative">
            <Search size={14} className="absolute left-2.5 top-1/2 -translate-y-1/2 text-muted-foreground" />
            <Input placeholder={t('usage.filter.account', { defaultValue: '筛选账号/厂商' })} value={accountFilter} onChange={e => setAccountFilter(e.target.value)} className="h-8 pl-8 w-56 text-xs" />
          </div>
        </div>
        <div className="overflow-x-auto">
          <table className="w-full text-xs">
            <thead className="bg-muted/50 text-muted-foreground">
              <tr>
                <th className="text-left px-4 py-2 cursor-pointer select-none" onClick={() => toggleAccountSort('name')}>{t('usage.tables.headers.account', { defaultValue: '账号' })}{sortIndicator(accountSort,'name')}</th>
                <th className="text-right px-3 py-2 cursor-pointer select-none" onClick={() => toggleAccountSort('input')}>{t('usage.input', { defaultValue: '输入' })}{sortIndicator(accountSort,'input')}</th>
                <th className="text-right px-3 py-2 cursor-pointer select-none" onClick={() => toggleAccountSort('output')}>{t('usage.output', { defaultValue: '输出' })}{sortIndicator(accountSort,'output')}</th>
                <th className="text-right px-3 py-2 cursor-pointer select-none" onClick={() => toggleAccountSort('cacheHit')}>{t('usage.cacheHit', { defaultValue: '缓存' })}{sortIndicator(accountSort,'cacheHit')}</th>
                <th className="text-right px-3 py-2 cursor-pointer select-none" onClick={() => toggleAccountSort('hitRate')}>{t('usage.tables.headers.hitRate', { defaultValue: '命中率' })}{sortIndicator(accountSort,'hitRate')}</th>
                <th className="text-right px-3 py-2 cursor-pointer select-none" onClick={() => toggleAccountSort('requests')}>{t('usage.tables.headers.requests', { defaultValue: '请求' })}{sortIndicator(accountSort,'requests')}</th>
                <th className="text-right px-3 py-2 cursor-pointer select-none" onClick={() => toggleAccountSort('successRate')}>{t('usage.tables.headers.successRate', { defaultValue: '成功率' })}{sortIndicator(accountSort,'successRate')}</th>
                <th className="text-right px-3 py-2 cursor-pointer select-none" onClick={() => toggleAccountSort('avgLatency')}>{t('usage.tables.headers.avgLatency', { defaultValue: '平均耗时' })}{sortIndicator(accountSort,'avgLatency')}</th>
                <th className="text-right px-3 py-2 cursor-pointer select-none" onClick={() => toggleAccountSort('avgTtft')}>{t('usage.tables.headers.avgTtft', { defaultValue: '平均 TTFT' })}{sortIndicator(accountSort,'avgTtft')}</th>
                <th className="text-right px-3 py-2 cursor-pointer select-none" onClick={() => toggleAccountSort('avgTps')}>{t('usage.tables.headers.avgTps', { defaultValue: 'Token/s' })}{sortIndicator(accountSort,'avgTps')}</th>
              </tr>
            </thead>
            <tbody className="divide-y divide-border/50">
              {sortedAccounts.length === 0 ? (
                <tr><td colSpan={10} className="py-8"><EmptyState icon={Inbox} title={t('usage.noData', { defaultValue: '暂无数据' })} /></td></tr>
              ) : sortedAccounts.map(a => (
                <tr key={a.id} className="hover:bg-muted/30">
                  <td className="px-4 py-2 font-medium">{a.name}</td>
                  <td className="text-right px-3 py-2">{fmtTokens(a.input)}</td>
                  <td className="text-right px-3 py-2">{fmtTokens(a.output)}</td>
                  <td className="text-right px-3 py-2">{fmtTokens(a.cacheRead ?? 0)}</td>
                  <td className="text-right px-3 py-2">{a.cacheHitRate.toFixed(1)}%</td>
                  <td className="text-right px-3 py-2">{a.requests}</td>
                  <td className="text-right px-3 py-2">{a.requests ? Math.round(a.successCount / a.requests * 100) : 0}%</td>
                  <td className="text-right px-3 py-2">{fmtSec(Math.max(0, a.avgLatency - a.avgTtft))}</td>
                  <td className="text-right px-3 py-2">{fmtSec(a.avgTtft)}</td>
                  <td className="text-right px-3 py-2">{a.avgTps.toFixed(1)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      </div>
    </div>
  );
}