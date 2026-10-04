import { useCallback, useEffect, useMemo, useState } from 'react';
import { apiFetch } from '@/lib/api';
import { useTranslation } from 'react-i18next';
import { Check, DollarSign, Inbox, Pencil, RefreshCw, X } from 'lucide-react';
import { PageHeader } from '../components/shared/PageHeader';
import { EmptyState } from '../components/shared/EmptyState';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { fmtPricePerMillion } from '../utils/format';

interface PriceRow {
  model_id: string;
  vendor: string | null;
  input_price: number | null;
  output_price: number | null;
  cache_read_price: number | null;
  cache_write_price: number | null;
  source: string;
  source_model_id: string | null;
  updated_at: string | null;
}

/// 「上游账号 × 模型」的真实价目（upstream_prices）。
interface UpstreamRow {
  accountId: number;
  alias: string | null;
  modelId: string;
  inputPrice: number | null;
  outputPrice: number | null;
  cacheReadPrice: number | null;
  cacheWritePrice: number | null;
  longContextThreshold: number | null;
  longInputPrice: number | null;
  longOutputPrice: number | null;
  source: string | null;
  updatedAt: string | null;
}

interface Draft {
  vendor: string;
  input: string;
  output: string;
  cacheRead: string;
  cacheWrite: string;
}

const EMPTY_DRAFT: Draft = { vendor: '', input: '', output: '', cacheRead: '', cacheWrite: '' };

// 单价列显示的是「美元 / 百万 token」。
const priceCell = (v: number | null): string =>
  v == null ? '—' : `$${fmtPricePerMillion(v)}`;

// 存储是美元 / token，输入框按美元 / 百万 token 显示 —— 否则 3e-7 会显示成
// 一连串 0，看着像免费。toPrecision(12) 抹掉浮点乘法的尾数噪声。
const toPerM = (v: number | null): string =>
  v == null ? '' : String(Number((v * 1_000_000).toPrecision(12)));

const toPerToken = (v: string): number | null => {
  if (v.trim() === '') return null;
  const n = Number(v);
  return Number.isFinite(n) ? n / 1_000_000 : null;
};

/// 价目表管理：看自动拉到的价、手工改价、触发刷新。
///
/// 单价一律按「美元 / 百万 token」展示与手填；库里存的是美元 / token。
/// 全局目录（model_prices）是兜底；真正用的是「上游账号 × 模型」（upstream_prices）。
export default function PricesPage() {
  const { t } = useTranslation();
  const [rows, setRows] = useState<PriceRow[]>([]);
  const [upstream, setUpstream] = useState<UpstreamRow[]>([]);
  const [unpriced, setUnpriced] = useState<string[]>([]);
  const [loading, setLoading] = useState(false);
  const [notice, setNotice] = useState('');
  const [editingId, setEditingId] = useState<string | null>(null);
  const [editingAccountId, setEditingAccountId] = useState<number | null>(null);
  const [draft, setDraft] = useState<Draft>(EMPTY_DRAFT);

  const load = useCallback(async () => {
    setLoading(true);
    try {
      const res = await apiFetch('/api/model-prices');
      const data = await res.json();
      setRows(data.prices ?? []);
      setUpstream(data.upstream ?? []);
      setUnpriced(data.unpriced ?? []);
    } catch {
      setNotice(t('prices.loadFailed', { defaultValue: '加载价目失败' }));
    } finally {
      setLoading(false);
    }
  }, [t]);

  useEffect(() => { load(); }, [load]);

  // 按上游账号分组展示。
  const byAccount = useMemo(() => {
    const m = new Map<number, { alias: string | null; rows: UpstreamRow[] }>();
    for (const r of upstream) {
      const e = m.get(r.accountId) ?? { alias: r.alias, rows: [] };
      e.rows.push(r);
      m.set(r.accountId, e);
    }
    return [...m.entries()].sort((a, b) => a[0] - b[0]);
  }, [upstream]);

  const refresh = async () => {
    setLoading(true);
    setNotice('');
    try {
      const res = await apiFetch('/api/model-prices/refresh', { method: 'POST' });
      const data = await res.json();
      if (!res.ok) {
        setNotice(data.error ?? 'refresh failed');
        return;
      }
      const r = data.report ?? {};
      const u = data.upstream ?? {};
      setNotice(t('prices.refreshDone', {
        defaultValue: 'OpenRouter {{fetched}} 条；按账号：匹配 {{matched}}，free {{free}}，未匹配 {{unmatched}}，人工 {{manual}}',
        fetched: r.fetched ?? 0,
        matched: u.matched ?? 0,
        free: u.free_rows ?? 0,
        unmatched: (u.unmatched ?? []).length,
        manual: (u.manual_accounts ?? []).length,
      }));
      await load();
    } catch {
      setNotice(t('prices.refreshFailed', { defaultValue: '刷新失败（外网不可达？）' }));
    } finally {
      setLoading(false);
    }
  };

  const beginEdit = (modelId: string, accountId: number | null, d: Draft) => {
    setEditingId(modelId);
    setEditingAccountId(accountId);
    setDraft(d);
  };

  const cancelEdit = () => {
    setEditingId(null);
    setEditingAccountId(null);
    setDraft(EMPTY_DRAFT);
  };

  const startEdit = (row: PriceRow) =>
    beginEdit(row.model_id, null, {
      vendor: row.vendor ?? '',
      input: toPerM(row.input_price ?? 0),
      output: toPerM(row.output_price ?? 0),
      cacheRead: toPerM(row.cache_read_price),
      cacheWrite: toPerM(row.cache_write_price),
    });

  const startEditUpstream = (row: UpstreamRow) =>
    beginEdit(row.modelId, row.accountId, {
      vendor: '',
      input: toPerM(row.inputPrice ?? 0),
      output: toPerM(row.outputPrice ?? 0),
      cacheRead: toPerM(row.cacheReadPrice),
      cacheWrite: toPerM(row.cacheWritePrice),
    });

  const save = async () => {
    if (!editingId) return;
    const inputPerM = Number(draft.input);
    const outputPerM = Number(draft.output);
    if (
      draft.input.trim() === '' ||
      draft.output.trim() === '' ||
      !Number.isFinite(inputPerM) ||
      !Number.isFinite(outputPerM)
    ) {
      setNotice(t('prices.invalidNumber', { defaultValue: '输入 / 输出价必须是数字' }));
      return;
    }
    setLoading(true);
    try {
      const res = await apiFetch('/api/model-prices', {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          modelId: editingId,
          ...(editingAccountId != null ? { accountId: editingAccountId } : {}),
          vendor: draft.vendor.trim() || null,
          inputPrice: inputPerM / 1_000_000,
          outputPrice: outputPerM / 1_000_000,
          cacheReadPrice: toPerToken(draft.cacheRead),
          cacheWritePrice: toPerToken(draft.cacheWrite),
        }),
      });
      const data = await res.json();
      if (!res.ok) {
        setNotice(data.error ?? 'save failed');
        return;
      }
      cancelEdit();
      await load();
    } catch {
      setNotice(t('prices.saveFailed', { defaultValue: '保存失败' }));
    } finally {
      setLoading(false);
    }
  };

  const editing = (id: string, accountId: number | null = null) =>
    editingId === id && editingAccountId === accountId;

  const editCells = (key: string) => (
    <>
      <td className="text-right px-3 py-2">
        <Input value={draft.input} onChange={e => setDraft(d => ({ ...d, input: e.target.value }))} className="h-7 w-24 text-xs text-right" key={`${key}-i`} />
      </td>
      <td className="text-right px-3 py-2">
        <Input value={draft.output} onChange={e => setDraft(d => ({ ...d, output: e.target.value }))} className="h-7 w-24 text-xs text-right" key={`${key}-o`} />
      </td>
      <td className="text-right px-3 py-2">
        <Input value={draft.cacheRead} onChange={e => setDraft(d => ({ ...d, cacheRead: e.target.value }))} className="h-7 w-24 text-xs text-right" key={`${key}-cr`} />
      </td>
      <td className="text-right px-3 py-2">
        <Input value={draft.cacheWrite} onChange={e => setDraft(d => ({ ...d, cacheWrite: e.target.value }))} className="h-7 w-24 text-xs text-right" key={`${key}-cw`} />
      </td>
    </>
  );

  const actionCell = (isEditing: boolean, onEdit: () => void) =>
    isEditing ? (
      <span className="inline-flex gap-1">
        <Button size="sm" variant="ghost" onClick={save} title={t('common.save', { defaultValue: '保存' })}><Check size={14} /></Button>
        <Button size="sm" variant="ghost" onClick={cancelEdit} title={t('common.cancel', { defaultValue: '取消' })}><X size={14} /></Button>
      </span>
    ) : (
      <Button size="sm" variant="ghost" onClick={onEdit} title={t('common.edit', { defaultValue: '编辑' })}><Pencil size={14} /></Button>
    );

  return (
    <div className="space-y-6">
      <PageHeader
        title={t('common.prices', { defaultValue: '价目表' })}
        subtitle={t('prices.subtitle', { defaultValue: '单价为「美元 / 百万 token」；手工定价不会被 6 小时刷新覆盖' })}
        icon={<DollarSign size={20} />}
        action={
          <Button size="sm" onClick={refresh} disabled={loading}>
            <RefreshCw size={14} className="mr-1.5" />
            {t('prices.refresh', { defaultValue: '立即刷新' })}
          </Button>
        }
      />

      {notice && (
        <div className="text-xs text-muted-foreground bg-muted/50 border border-border rounded-lg px-3 py-2">{notice}</div>
      )}

      {unpriced.length > 0 && (
        <div className="bg-warning/10 border border-warning/20 rounded-xl p-4 text-xs">
          <div className="font-semibold mb-2">
            {t('prices.unpricedTitle', { defaultValue: '{{count}} 个模型有流量但没有价目，成本按 0 计', count: unpriced.length })}
          </div>
          <div className="flex flex-wrap gap-1.5">
            {unpriced.map(m => (
              <span key={m} className="px-2 py-0.5 rounded bg-background border border-border font-mono">{m}</span>
            ))}
          </div>
        </div>
      )}

      {/* 按上游账号的真实价目 —— 成本估算实际用的是这批 */}
      <div className="space-y-3">
        <h3 className="text-sm font-bold">{t('prices.byAccount', { defaultValue: '按上游账号（实际计价）' })}</h3>
        {byAccount.length === 0 ? (
          <div className="bg-card border border-border rounded-xl py-8">
            <EmptyState icon={Inbox} title={t('usage.noData', { defaultValue: '暂无数据' })} />
          </div>
        ) : byAccount.map(([accountId, g]) => (
          <div key={accountId} className="bg-card border border-border rounded-xl overflow-hidden">
            <div className="px-4 py-2 border-b border-border/50 flex items-center gap-2">
              <span className="font-mono text-xs text-muted-foreground">#{accountId}</span>
              <span className="text-sm font-semibold">{g.alias ?? '—'}</span>
              <span className="text-xs text-muted-foreground">· {g.rows.length}</span>
            </div>
            <div className="overflow-x-auto">
              <table className="w-full text-xs">
                <thead className="bg-muted/50 text-muted-foreground">
                  <tr>
                    <th className="text-left px-4 py-2">{t('prices.headers.model', { defaultValue: '模型' })}</th>
                    <th className="text-left px-3 py-2">{t('prices.headers.source', { defaultValue: '来源' })}</th>
                    <th className="text-right px-3 py-2">{t('prices.headers.inputPerM', { defaultValue: '输入 $/M' })}</th>
                    <th className="text-right px-3 py-2">{t('prices.headers.outputPerM', { defaultValue: '输出 $/M' })}</th>
                    <th className="text-right px-3 py-2">{t('prices.headers.cacheReadPerM', { defaultValue: '缓存读 $/M' })}</th>
                    <th className="text-right px-3 py-2">{t('prices.headers.cacheWritePerM', { defaultValue: '缓存写 $/M' })}</th>
                    <th className="text-left px-3 py-2">{t('prices.headers.tier', { defaultValue: '长上下文' })}</th>
                    <th className="text-right px-4 py-2">{t('prices.headers.action', { defaultValue: '操作' })}</th>
                  </tr>
                </thead>
                <tbody className="divide-y divide-border/50">
                  {g.rows.map(row => {
                    const isEditing = editing(row.modelId, row.accountId);
                    const key = `${row.accountId}:${row.modelId}`;
                    return (
                      <tr key={key} className="hover:bg-muted/30">
                        <td className="px-4 py-2 font-mono max-w-[240px] truncate">{row.modelId}</td>
                        <td className={`px-3 py-2 ${row.source === 'manual' ? 'text-warning' : 'text-muted-foreground'}`}>{row.source ?? '—'}</td>
                        {isEditing
                          ? editCells(key)
                          : (
                            <>
                              <td className="text-right px-3 py-2">{priceCell(row.inputPrice)}</td>
                              <td className="text-right px-3 py-2">{priceCell(row.outputPrice)}</td>
                              <td className="text-right px-3 py-2">{priceCell(row.cacheReadPrice)}</td>
                              <td className="text-right px-3 py-2">{priceCell(row.cacheWritePrice)}</td>
                            </>
                          )}
                        <td className="px-3 py-2 text-muted-foreground">
                          {row.longContextThreshold != null
                            ? `>${row.longContextThreshold.toLocaleString()} → $${fmtPricePerMillion(row.longInputPrice)}`
                            : '—'}
                        </td>
                        <td className="text-right px-4 py-2">{actionCell(isEditing, () => startEditUpstream(row))}</td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>
            </div>
          </div>
        ))}
      </div>

      {/* 全局兜底目录 */}
      <div className="space-y-3">
        <h3 className="text-sm font-bold">{t('prices.globalCatalog', { defaultValue: '全局兜底目录（OpenRouter）' })}</h3>
        <div className="bg-card border border-border rounded-xl overflow-hidden">
          <div className="overflow-x-auto">
            <table className="w-full text-xs">
              <thead className="bg-muted/50 text-muted-foreground">
                <tr>
                  <th className="text-left px-4 py-2">{t('prices.headers.model', { defaultValue: '模型' })}</th>
                  <th className="text-left px-3 py-2">{t('prices.headers.vendor', { defaultValue: '厂商' })}</th>
                  <th className="text-left px-3 py-2">{t('prices.headers.source', { defaultValue: '来源' })}</th>
                  <th className="text-right px-3 py-2">{t('prices.headers.inputPerM', { defaultValue: '输入 $/M' })}</th>
                  <th className="text-right px-3 py-2">{t('prices.headers.outputPerM', { defaultValue: '输出 $/M' })}</th>
                  <th className="text-right px-3 py-2">{t('prices.headers.cacheReadPerM', { defaultValue: '缓存读 $/M' })}</th>
                  <th className="text-right px-3 py-2">{t('prices.headers.cacheWritePerM', { defaultValue: '缓存写 $/M' })}</th>
                  <th className="text-right px-3 py-2">{t('prices.headers.updated', { defaultValue: '更新时间' })}</th>
                  <th className="text-right px-4 py-2">{t('prices.headers.action', { defaultValue: '操作' })}</th>
                </tr>
              </thead>
              <tbody className="divide-y divide-border/50">
                {rows.length === 0 ? (
                  <tr><td colSpan={9} className="py-8"><EmptyState icon={Inbox} title={t('usage.noData', { defaultValue: '暂无数据' })} /></td></tr>
                ) : rows.map(row => {
                  const isEditing = editing(row.model_id, null);
                  return (
                    <tr key={row.model_id} className="hover:bg-muted/30">
                      <td className="px-4 py-2 font-mono max-w-[260px] truncate">{row.model_id}</td>
                      <td className="px-3 py-2">
                        {isEditing
                          ? <Input value={draft.vendor} onChange={e => setDraft(d => ({ ...d, vendor: e.target.value }))} className="h-7 w-28 text-xs" />
                          : (row.vendor ?? '—')}
                      </td>
                      <td className={`px-3 py-2 ${row.source === 'manual' ? 'text-warning' : 'text-muted-foreground'}`}>{row.source}</td>
                      {isEditing
                        ? editCells(row.model_id)
                        : (
                          <>
                            <td className="text-right px-3 py-2">{priceCell(row.input_price)}</td>
                            <td className="text-right px-3 py-2">{priceCell(row.output_price)}</td>
                            <td className="text-right px-3 py-2">{priceCell(row.cache_read_price)}</td>
                            <td className="text-right px-3 py-2">{priceCell(row.cache_write_price)}</td>
                          </>
                        )}
                      <td className="text-right px-3 py-2 text-muted-foreground">{row.updated_at ?? '—'}</td>
                      <td className="text-right px-4 py-2">{actionCell(isEditing, () => startEdit(row))}</td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>
        </div>
      </div>
    </div>
  );
}
