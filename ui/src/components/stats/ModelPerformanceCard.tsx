import React, { useEffect, useMemo, useState } from 'react';
import { apiFetch } from '@/lib/api';
import { useTranslation } from 'react-i18next';
import { AlertTriangle, Check, Inbox } from 'lucide-react';
import {
  Chart as ChartJS,
  CategoryScale,
  LinearScale,
  PointElement,
  LineElement,
  Tooltip,
  Legend,
  Filler,
  type ChartData,
  type ChartOptions,
} from 'chart.js';
import { Line } from 'react-chartjs-2';
import FilterSelect from '@/components/shared/FilterSelect';
import { EmptyState } from '@/components/shared/EmptyState';
import { seriesColor } from '@/utils/thresholds';

// stats.tsx 也会注册；这里重复注册是幂等的，保证卡片被单独引入时也能画。
ChartJS.register(CategoryScale, LinearScale, PointElement, LineElement, Tooltip, Legend, Filler);

export interface PerfRow {
  model: string | null;
  accountId: number | null;
  accountName: string | null;
  streamRequests: number;
  successRate: number;
  avgTtft: number;
  p95Ttft: number;
  avgTps: number;
  ttftSamples: number;
  tpsSamples: number;
  prevStreamRequests: number;
  prevSuccessRate: number;
  prevAvgTtft: number;
  prevP95Ttft: number;
  prevAvgTps: number;
  deltaTtftPct: number;
  deltaTpsPct: number;
  deltaSuccessRate: number;
}

export interface PerfPoint {
  model: string | null;
  accountId: number | null;
  bucket: number;
  streamRequests: number;
  avgTtft: number;
  p95Ttft: number;
  avgTps: number;
  ttftSamples: number;
  tpsSamples: number;
}

export interface LivePerf {
  accountId: number;
  model: string;
  severity: number | null;
  ttftFastMs: number | null;
  tpsFast: number | null;
  samples: number;
  lastSampleAgeSecs: number;
}

interface PerformanceResponse {
  groupBy: string;
  granularityMs: number;
  rows: PerfRow[];
  series: PerfPoint[];
  live: LivePerf[];
}

interface ModelPerformanceCardProps {
  start: number;
  end: number;
  keyId: number | null;
  accountId: number | null;
  alias: string | null;
  aliasKind: 'ordinary' | 'aggregate' | null;
  /** 选聚合别名时先按配置顺序列候选，方便和分流卡对上。 */
  candidates?: Array<{ accountId: number; model: string; accountName?: string | null }>;
  /** 聚合别名的当前活跃候选下标，默认选中它。 */
  activeIndex?: number;
}

const keyOf = (accountId: number | null, model: string | null) =>
  `${accountId === null ? '' : accountId}|${model ?? ''}`;

function DeltaChip({ label, value, goodWhenPositive }: { label: string; value: number; goodWhenPositive: boolean }) {
  if (!Number.isFinite(value) || Math.abs(value) < 0.005) {
    return <span className="text-[11px] px-1.5 py-0.5 rounded border border-border text-muted-foreground">{label} —</span>;
  }
  const good = goodWhenPositive ? value > 0 : value < 0;
  return (
    <span className={`text-[11px] px-1.5 py-0.5 rounded border ${
      good ? 'border-success/30 bg-success/10 text-success' : 'border-destructive/30 bg-destructive/10 text-destructive'
    }`}>
      {label} {value > 0 ? '+' : ''}{value.toFixed(1)}%
    </span>
  );
}

function SeverityBadge({ live, detail }: { live: LivePerf | undefined; detail?: string }) {
  const { t } = useTranslation();
  if (!live || live.severity === null) {
    return <span className="text-[11px] text-muted-foreground inline-flex items-center gap-1" title={detail}><Inbox size={11} />{t('usage.perf.noSample', { defaultValue: '无样本' })}</span>;
  }
  if (live.severity > 1.3) {
    return <span className="text-[11px] text-warning inline-flex items-center gap-1" title={detail}><AlertTriangle size={11} />{t('usage.perf.slow', { defaultValue: '变慢' })} {live.severity.toFixed(1)}×</span>;
  }
  return <span className="text-[11px] text-success inline-flex items-center gap-1" title={detail}><Check size={11} />{t('usage.perf.normal', { defaultValue: '正常' })} {live.severity.toFixed(1)}×</span>;
}

/** 流式性能：多条时间序列 + 当前/前一窗口变化 + 实时 severity。 */
export default function ModelPerformanceCard({
  start,
  end,
  keyId,
  accountId,
  alias,
  aliasKind,
  candidates,
  activeIndex,
}: ModelPerformanceCardProps) {
  const { t } = useTranslation();
  const [data, setData] = useState<PerformanceResponse | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [selected, setSelected] = useState<string | null>(null);

  const groupBy = aliasKind === 'aggregate' ? 'account_model' : 'model';
  useEffect(() => {
    const ac = new AbortController();
    const q = new URLSearchParams({
      start: String(start),
      end: String(end),
      group_by: groupBy,
    });
    if (keyId !== null) q.set('key_id', String(keyId));
    if (accountId !== null) q.set('account_id', String(accountId));
    if (alias) {
      q.set('alias', alias);
      q.set('alias_kind', aliasKind ?? 'ordinary');
    }
    apiFetch(`/api/stats/performance?${q.toString()}`, { signal: ac.signal })
      .then(async res => {
        if (!res.ok) throw new Error(await res.text());
        setData(await res.json() as PerformanceResponse);
        setError(null);
      })
      .catch((e: unknown) => {
        if ((e as Error)?.name !== 'AbortError') setError(String(e));
      });
    return () => ac.abort();
  }, [start, end, keyId, accountId, alias, aliasKind, groupBy]);

  // 选项固定按行顺序（聚合时先列配置候选），筛选不会给幸存者换色。
  const options = useMemo(() => {
    const rows = data?.rows ?? [];
    const seen = new Set<string>();
    const out: Array<{ value: string; label: string }> = [];
    if (aliasKind === 'aggregate') {
      for (const c of candidates ?? []) {
        const key = keyOf(c.accountId, c.model);
        if (seen.has(key)) continue;
        seen.add(key);
        out.push({
          value: key,
          label: `${c.accountName ? `${c.accountName} / ` : c.accountId === null ? '' : `#${c.accountId} `}${c.model ?? '—'}`,
        });
      }
    }
    for (const r of rows) {
      const key = keyOf(r.accountId, r.model);
      if (seen.has(key)) continue;
      seen.add(key);
      out.push({
        value: key,
        label: groupBy === 'account_model'
          ? `${r.accountName ?? (r.accountId === null ? '—' : `#${r.accountId}`)} / ${r.model ?? '—'}`
          : r.model ?? '—',
      });
    }
    return out;
  }, [data, aliasKind, candidates, groupBy]);

  // 默认值：聚合别名选当前活跃候选，其余选请求量最高的模型；用户手动切换后不再覆盖。
  useEffect(() => {
    if (!data?.rows.length || (selected !== null && options.some(o => o.value === selected))) return;
    let fallback: string | null = null;
    if (aliasKind === 'aggregate' && activeIndex !== undefined) {
      const c = candidates?.[activeIndex];
      if (c) fallback = keyOf(c.accountId, c.model);
    }
    if (!fallback || !options.some(o => o.value === fallback)) {
      const top = [...data.rows].sort((a, b) => b.streamRequests - a.streamRequests)[0];
      fallback = top ? keyOf(top.accountId, top.model) : null;
    }
    setSelected(fallback);
  }, [data, options, selected, aliasKind, activeIndex, candidates]);

  const visibleRows = useMemo(
    () => (data?.rows ?? []).filter(r => selected === null || keyOf(r.accountId, r.model) === selected),
    [data, selected],
  );
  const visiblePoints = useMemo(
    () => (data?.series ?? []).filter(p => selected === null || keyOf(p.accountId, p.model) === selected),
    [data, selected],
  );
  const liveMap = useMemo(() => {
    const map = new Map<string, LivePerf>();
    for (const n of data?.live ?? []) map.set(keyOf(n.accountId, n.model), n);
    return map;
  }, [data]);

  // group_by=model 时行里没有 accountId：按模型取所有账户中 severity 最大的一条，
  // tooltip 列出参与合并的账户，避免把多个账户合成一个含糊的数却不交代来源。
  function liveFor(row: PerfRow): LivePerf | undefined {
    if (groupBy !== 'model') return liveMap.get(keyOf(row.accountId, row.model));
    return (data?.live ?? []).filter(n => n.model === row.model)
      .reduce<LivePerf | undefined>((best, n) =>
        !best || (n.severity ?? 0) > (best.severity ?? 0) ? n : best, undefined);
  }

  function liveDetail(row: PerfRow, live: LivePerf | undefined): string | undefined {
    if (!live) return undefined;
    const nodes = groupBy === 'model'
      ? (data?.live ?? []).filter(n => n.model === row.model)
      : [live];
    const accounts = nodes.map(n => `#${n.accountId}`).join(', ');
    return `${accounts ? `${accounts} · ` : ''}${live.samples} ${t('usage.perf.samples', { defaultValue: '样本' })} · ${live.lastSampleAgeSecs}s`;
  }

  const buckets = useMemo(
    () => [...new Set(visiblePoints.map(p => p.bucket))].sort((a, b) => a - b),
    [visiblePoints],
  );

  function pointsFor(metric: 'avgTtft' | 'p95Ttft' | 'avgTps'): Array<number | null> {
    return buckets.map(b => {
      const matching = visiblePoints.filter(p => p.bucket === b);
      if (!matching.length) return null;
      return matching.reduce((s, p) => s + p[metric], 0) / matching.length;
    });
  }

  const labels = buckets.map(b => new Date(b).toLocaleString());
  const ttftData: ChartData<'line'> = {
    labels,
    datasets: [
      {
        label: t('usage.perf.avgTtft', { defaultValue: '平均 TTFT (s)' }),
        data: pointsFor('avgTtft'),
        borderColor: seriesColor(0), backgroundColor: seriesColor(0),
        borderWidth: 2, pointRadius: 4, tension: 0.25,
      },
      {
        label: t('usage.perf.p95Ttft', { defaultValue: 'P95 TTFT (s)' }),
        data: pointsFor('p95Ttft'),
        borderColor: seriesColor(3), backgroundColor: seriesColor(3),
        borderWidth: 2, pointRadius: 4, tension: 0.25,
      },
    ],
  };
  const tpsData: ChartData<'line'> = {
    labels,
    datasets: [{
      label: t('usage.perf.avgTps', { defaultValue: '平均 Token/s' }),
      data: pointsFor('avgTps'),
      borderColor: seriesColor(1), backgroundColor: seriesColor(1),
      borderWidth: 2, pointRadius: 4, tension: 0.25,
    }],
  };
  const lineOpts = useMemo<ChartOptions<'line'>>(() => ({
    responsive: true,
    maintainAspectRatio: false,
    interaction: { mode: 'index', intersect: false },
    plugins: { legend: { display: true, labels: { boxWidth: 10, font: { size: 10 } } } },
    scales: {
      x: { ticks: { font: { size: 9 }, maxTicksLimit: 8, maxRotation: 0 } },
      y: { beginAtZero: true, ticks: { font: { size: 9 } }, grid: { color: 'rgba(148,163,184,0.15)' } },
    },
  }), []);
  const tpsOpts = useMemo<ChartOptions<'line'>>(() => ({
    ...lineOpts,
    plugins: { ...lineOpts.plugins, legend: { display: false } },
  }), [lineOpts]);

  return (
    <section className="bg-card border border-border rounded-xl p-4">
      <div className="flex flex-wrap items-center gap-2 mb-3">
        <h3 className="text-sm font-bold">{t('usage.perf.title', { defaultValue: '模型性能变化' })}</h3>
        <FilterSelect
          value={selected}
          options={options}
          allLabel={t('usage.perf.allModels', { defaultValue: '全部模型' })}
          onChange={setSelected}
        />
        <span className="text-[11px] text-muted-foreground">
          {t('usage.perf.scope', { defaultValue: '曲线只用合格成功样本 · 表格列出样本数' })}
        </span>
      </div>

      {error ? (
        <p className="text-xs text-destructive">{t('usage.perf.loadError', { defaultValue: '性能数据加载失败' })}: {error}</p>
      ) : !data || data.rows.length === 0 ? (
        <EmptyState icon={Inbox} title={t('usage.noData', { defaultValue: '暂无数据' })} />
      ) : (
        <>
          <div className="grid grid-cols-1 lg:grid-cols-2 gap-4">
            <div className="h-56">
              <p className="text-xs text-muted-foreground mb-1">{t('usage.perf.ttftChart', { defaultValue: '首字节耗时' })}</p>
              <Line data={ttftData} options={lineOpts} />
            </div>
            <div className="h-56">
              <p className="text-xs text-muted-foreground mb-1">{t('usage.perf.tpsChart', { defaultValue: '生成速度' })}</p>
              <Line data={tpsData} options={tpsOpts} />
            </div>
          </div>
          <div className="overflow-x-auto mt-3">
            <table className="w-full text-xs">
              <thead className="bg-muted/50 text-muted-foreground">
                <tr>
                  <th className="text-left px-3 py-2">{t('usage.tables.headers.model', { defaultValue: '模型 / 候选' })}</th>
                  <th className="text-right px-3 py-2">{t('usage.tables.headers.streamRequests', { defaultValue: '流式请求' })}</th>
                  <th className="text-right px-3 py-2">{t('usage.tables.headers.successRate', { defaultValue: '成功率' })}</th>
                  <th className="text-right px-3 py-2">TTFT</th>
                  <th className="text-right px-3 py-2">TPS</th>
                  <th className="text-left px-3 py-2">{t('usage.perf.change', { defaultValue: '对比前一窗口' })}</th>
                  <th className="text-left px-3 py-2">{t('usage.perf.live', { defaultValue: '实时' })}</th>
                </tr>
              </thead>
              <tbody className="divide-y divide-border/50">
                {visibleRows.map(r => {
                  const key = keyOf(r.accountId, r.model);
                  const liveNode = liveFor(r);
                  return (
                    <tr key={key} className="hover:bg-muted/30">
                      <td className="px-3 py-2 font-mono truncate max-w-[240px]">
                        {r.accountId !== null ? `#${r.accountId} / ` : ''}{r.model ?? '—'}
                      </td>
                      <td className="text-right px-3 py-2">{r.streamRequests}</td>
                      <td className="text-right px-3 py-2">{(r.successRate * 100).toFixed(1)}%</td>
                      <td className="text-right px-3 py-2" title={t('usage.perf.ttftSamplesTip', { defaultValue: 'TTFT 只统计成功且非空样本' })}>
                        {r.avgTtft.toFixed(2)}s <span className="text-muted-foreground">p95 {r.p95Ttft.toFixed(2)} · n={r.ttftSamples}</span>
                      </td>
                      <td className="text-right px-3 py-2" title={t('usage.perf.tpsSamplesTip', { defaultValue: 'TPS 只统计解码段 ≥500ms 且输出 ≥8 token 的成功请求' })}>
                        {r.avgTps.toFixed(1)} <span className="text-muted-foreground">n={r.tpsSamples}</span>
                      </td>
                      <td className="px-3 py-2"><div className="flex flex-wrap gap-1">
                        <DeltaChip label="TTFT" value={r.deltaTtftPct} goodWhenPositive={false} />
                        <DeltaChip label="TPS" value={r.deltaTpsPct} goodWhenPositive />
                        <DeltaChip label={t('usage.perf.success', { defaultValue: '成功率' })} value={r.deltaSuccessRate * 100} goodWhenPositive />
                      </div></td>
                      <td className="px-3 py-2"><div className="flex flex-col">
                        <SeverityBadge live={liveNode} detail={liveDetail(r, liveNode)} />
                        {liveNode && <span className="text-[10px] text-muted-foreground">{liveNode.samples} {t('usage.perf.samples', { defaultValue: '样本' })} · {liveNode.lastSampleAgeSecs}s</span>}
                      </div></td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>
        </>
      )}
    </section>
  );
}
