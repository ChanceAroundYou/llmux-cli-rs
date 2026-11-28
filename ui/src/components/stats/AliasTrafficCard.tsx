import React, { useEffect, useMemo, useState } from 'react';
import { apiFetch } from '@/lib/api';
import { useTranslation } from 'react-i18next';
import { AlertTriangle, Check, Inbox } from 'lucide-react';
import { EmptyState } from '@/components/shared/EmptyState';
import { seriesColor } from '@/utils/thresholds';
import type { LivePerf } from './ModelPerformanceCard';

export interface CandidateRow {
  accountId: number | null;
  accountName: string | null;
  model: string | null;
  candidateIdx: number | null;
  requests: number;
  successCount: number;
  avgTtft: number;
  avgTps: number;
}

export interface AggregateRuntime {
  active: number;
  degraded: boolean;
  splitAcc: number;
  adaptiveSplit: boolean;
  adaptiveMaxSpillPct: number;
  candidates: Array<{ account_id?: number; model?: string }>;
}

interface AliasTrafficCardProps {
  alias: string;
  aliasKind: 'ordinary' | 'aggregate';
  rows: CandidateRow[];
  aggregate?: AggregateRuntime;
  accountId: number | null;
  /** /api/stats 的 total_requests；用来区分「旧数据没有归属」与「本来就没流量」。 */
  summaryRequests: number;
}

function pct(part: number, total: number): string {
  return total > 0 ? `${(part / total * 100).toFixed(1)}%` : '0%';
}

const entityKey = (accountId: number | null | undefined, model: string | null | undefined) =>
  `${accountId ?? ''}|${model ?? ''}`;

function LiveCell({ live, detail }: { live?: LivePerf; detail?: string }) {
  const { t } = useTranslation();
  if (!live || live.severity === null) {
    return <span className="text-[11px] text-muted-foreground inline-flex items-center gap-1" title={detail}>
      <Inbox size={11} />{t('usage.perf.noSample', { defaultValue: '无样本' })}
    </span>;
  }
  const slow = live.severity > 1.3;
  const Icon = slow ? AlertTriangle : Check;
  return (
    <span className={`text-[11px] inline-flex items-center gap-1 ${slow ? 'text-warning' : 'text-success'}`} title={detail}>
      <Icon size={11} />
      {slow ? t('usage.perf.slow', { defaultValue: '偏慢' }) : t('usage.perf.normal', { defaultValue: '正常' })}
      {' '}{live.severity.toFixed(1)}×
    </span>
  );
}

/** 别名流量走向：按成功数分摊到 (账户, 模型, 候选下标)。 */
export default function AliasTrafficCard({
  alias,
  aliasKind,
  rows,
  aggregate,
  accountId,
  summaryRequests,
}: AliasTrafficCardProps) {
  const { t } = useTranslation();
  const [live, setLive] = useState<LivePerf[]>([]);
  const totalSuccess = rows.reduce((s, r) => s + (r.successCount || 0), 0);
  const noAttribution = rows.length === 0 && summaryRequests > 0;

  useEffect(() => {
    const ac = new AbortController();
    const q = new URLSearchParams({ alias, alias_kind: aliasKind });
    if (accountId !== null) q.set('account_id', String(accountId));
    apiFetch(`/api/stats/perf?${q.toString()}`, { signal: ac.signal })
      .then(async res => {
        if (!res.ok) throw new Error(await res.text());
        const body = await res.json() as { live?: LivePerf[] };
        setLive(Array.isArray(body.live) ? body.live : []);
      })
      .catch(() => setLive([]));
    return () => ac.abort();
  }, [alias, aliasKind, accountId]);

  const configured = aggregate?.candidates ?? [];
  const known = useMemo(
    () => new Set(configured.map(c => entityKey(c.account_id, c.model))),
    [configured],
  );
  // 历史行按当前候选配置排列；配置里没有的 (账户, 模型) 追加到末尾并标记「已移除」。
  const ordered = useMemo(() => {
    const used = new Set<number>();
    const out: Array<{ row: CandidateRow; idx: number }> = [];
    const take = (idx: number) => {
      if (used.has(idx)) return;
      used.add(idx);
      out.push({ row: rows[idx], idx });
    };
    if (aliasKind === 'aggregate') {
      for (const c of configured) {
        const idx = rows.findIndex((r, i) =>
          !used.has(i) && r.accountId === c.account_id && r.model === c.model);
        if (idx >= 0) take(idx);
      }
    }
    rows.forEach((_, i) => take(i));
    return out;
  }, [rows, aliasKind, configured]);

  const liveMap = useMemo(() => {
    const map = new Map<string, LivePerf>();
    for (const n of live) map.set(entityKey(n.accountId, n.model), n);
    return map;
  }, [live]);

  const activeCandidate = aggregate && configured[aggregate.active];
  const activeLabel = activeCandidate
    ? `${activeCandidate.model ?? '—'}${activeCandidate.account_id !== undefined ? ` · #${activeCandidate.account_id}` : ''}`
    : '—';

  return (
    <section className="bg-card border border-border rounded-xl p-4">
      <div className="flex flex-wrap items-center gap-2 mb-3">
        <h3 className="text-sm font-bold">{t('usage.aliasTraffic.title', { defaultValue: '别名流量走向' })}</h3>
        <span className="text-xs px-2 py-0.5 rounded border border-border text-muted-foreground font-mono">{alias}</span>
        <span className="text-xs px-2 py-0.5 rounded border border-primary/30 bg-primary/10 text-primary">
          {aliasKind === 'aggregate'
            ? t('usage.aliasTraffic.aggregate', { defaultValue: '聚合别名' })
            : t('usage.aliasTraffic.ordinary', { defaultValue: '普通别名' })}
        </span>
        {aggregate && (
          <>
            <span className="text-xs px-2 py-0.5 rounded border border-border text-muted-foreground">
              {t('usage.aliasTraffic.active', { defaultValue: '活跃' })} #{aggregate.active + 1} {activeLabel}
            </span>
            <span className="text-xs px-2 py-0.5 rounded border border-border text-muted-foreground">
              {t('usage.aliasTraffic.adaptive', { defaultValue: '自适应分流' })} {aggregate.adaptiveSplit
                ? t('common.on', { defaultValue: '开' })
                : t('common.off', { defaultValue: '关' })}
            </span>
            <span
              className={`text-xs px-2 py-0.5 rounded border ${
                aggregate.degraded
                  ? 'border-warning/30 bg-warning/10 text-warning'
                  : 'border-border text-muted-foreground'
              }`}
              title={`split_acc=${aggregate.splitAcc.toFixed(3)}`}
            >
              {aggregate.degraded
                ? `${t('usage.aliasTraffic.spilling', { defaultValue: '分流中' })} · ${t('usage.aliasTraffic.spillCap', { defaultValue: '上限' })} ${aggregate.adaptiveMaxSpillPct}%`
                : t('usage.aliasTraffic.noSpill', { defaultValue: '未分流' })}
            </span>
          </>
        )}
      </div>

      {noAttribution ? (
        <div className="py-6">
          <EmptyState
            icon={Inbox}
            title={t('usage.aliasTraffic.noAttribution', { defaultValue: '该时间段没有带别名归属的请求' })}
            description={t('usage.aliasTraffic.noAttributionDesc', {
              defaultValue: '归属列是新增的；旧请求不会回填。换个时间范围或发一条新请求后再看。',
            })}
          />
        </div>
      ) : rows.length === 0 ? (
        <EmptyState icon={Inbox} title={t('usage.noData', { defaultValue: '暂无数据' })} />
      ) : (
        <>
          {/* 占比条：宽度=成功数占比；颜色跟随候选配置顺序，不随筛选后的排名重排。
              色块间 2px 透明描边留出 surface gap，完整数值在下方表格里。 */}
          <div className="flex h-4 w-full overflow-hidden rounded bg-muted/40" role="img"
            aria-label={t('usage.aliasTraffic.shareBar', { defaultValue: '候选成功请求占比' })}>
            {ordered.map(({ row: r, idx }, i) => (
              <div
                key={idx}
                className="h-full border-x-2 border-transparent first:border-l-0 last:border-r-0 flex items-center justify-center text-[10px] text-white overflow-hidden whitespace-nowrap"
                style={{ width: pct(r.successCount || 0, totalSuccess), backgroundColor: seriesColor(i) }}
                title={`${r.model ?? '—'} · ${r.accountName ?? `#${r.accountId ?? '—'}`}: ${pct(r.successCount || 0, totalSuccess)}`}
              >
                {(r.successCount || 0) / Math.max(totalSuccess, 1) >= 0.12
                  ? pct(r.successCount || 0, totalSuccess) : null}
              </div>
            ))}
          </div>
          <div className="overflow-x-auto mt-3">
            <table className="w-full text-xs">
              <thead className="bg-muted/50 text-muted-foreground">
                <tr>
                  <th className="text-right px-3 py-2">#</th>
                  <th className="text-left px-3 py-2">{t('usage.tables.headers.model', { defaultValue: '模型' })}</th>
                  <th className="text-left px-3 py-2">{t('usage.tables.headers.account', { defaultValue: '账号' })}</th>
                  <th className="text-right px-3 py-2">{t('usage.tables.headers.requests', { defaultValue: '请求' })}</th>
                  <th className="text-right px-3 py-2">{t('usage.tables.headers.successRequests', { defaultValue: '成功请求' })}</th>
                  <th className="text-right px-3 py-2">{t('usage.tables.headers.successRate', { defaultValue: '成功率' })}</th>
                  <th className="text-right px-3 py-2">{t('usage.aliasTraffic.share', { defaultValue: '承接占比' })}</th>
                  <th className="text-right px-3 py-2">{t('usage.tables.headers.avgTtft', { defaultValue: '平均 TTFT' })}</th>
                  <th className="text-right px-3 py-2">{t('usage.tables.headers.avgTps', { defaultValue: 'Token/s' })}</th>
                  <th className="text-left px-3 py-2">{t('usage.perf.live', { defaultValue: '实时' })}</th>
                </tr>
              </thead>
              <tbody className="divide-y divide-border/50">
                {ordered.map(({ row: r, idx }, i) => {
                  const liveNode = liveMap.get(entityKey(r.accountId, r.model));
                  const removed = aliasKind === 'aggregate' && !known.has(entityKey(r.accountId, r.model));
                  const detail = liveNode
                    ? `${liveNode.model} · ${liveNode.samples} ${t('usage.perf.samples', { defaultValue: '样本' })} · ${liveNode.lastSampleAgeSecs}s`
                    : undefined;
                  return (
                    <tr key={idx} className="hover:bg-muted/30">
                      <td className="text-right px-3 py-2 font-mono">{r.candidateIdx ?? '—'}</td>
                      <td className="px-3 py-2 font-mono truncate max-w-[220px]">
                        <span className="inline-block w-2 h-2 rounded-sm mr-2 align-middle" style={{ backgroundColor: seriesColor(i) }} />
                        {r.model ?? '—'}
                        {removed && <span className="ml-1 text-[10px] text-muted-foreground">{t('usage.aliasTraffic.removed', { defaultValue: '已移除' })}</span>}
                      </td>
                      <td className="px-3 py-2">{r.accountName ?? (r.accountId !== null ? `#${r.accountId}` : '—')}</td>
                      <td className="text-right px-3 py-2">{r.requests}</td>
                      <td className="text-right px-3 py-2">{r.successCount}</td>
                      <td className="text-right px-3 py-2">{r.requests ? (r.successCount / r.requests * 100).toFixed(1) : '0.0'}%</td>
                      <td className="text-right px-3 py-2">{pct(r.successCount || 0, totalSuccess)}</td>
                      <td className="text-right px-3 py-2">{r.avgTtft?.toFixed(2) ?? '0.00'}s</td>
                      <td className="text-right px-3 py-2">{r.avgTps?.toFixed(1) ?? '0.0'}</td>
                      <td className="px-3 py-2"><LiveCell live={liveNode} detail={detail} /></td>
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
