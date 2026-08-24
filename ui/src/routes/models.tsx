import React, { useEffect, useState, useMemo } from 'react';
import { apiFetch } from "@/lib/api";
import { useModelsStore } from '../stores/models';
import {
  Box,
  Search,
  RefreshCcw,
  ChevronRight,
  Plus,
  Save,
  Trash2,
  LayoutGrid,
  Zap,
  ArrowRight,
  Copy,
  Layers,
  ChevronUp,
  ChevronDown
} from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { Dialog, ConfirmDialog } from '../components/Modal';
import { CopyButton } from '../components/CopyButton';
import { parseServerDate } from '../utils/date';
import { cn } from '../lib/utils'
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { Tabs, TabsList, TabsTrigger } from '@/components/ui/tabs';

function formatContextLength(n: number): string {
  if (n >= 1_000_000) {
    const m = n / 1_000_000;
    return Number.isInteger(m) ? `${m}M` : `${m.toFixed(1)}M`;
  }
  if (n >= 1_000) {
    const k = n / 1_000;
    return Number.isInteger(k) ? `${k}K` : `${k.toFixed(1)}K`;
  }
  return `${n}`;
}

// 下游模式（downstreamMode）→ 协议；default 表示尽可能透传（不过滤账户）
const DOWNSTREAM_MODES = ['default', 'chat', 'responses', 'messages'];
const normalizeMode = (v: any): string => (DOWNSTREAM_MODES.includes(v) ? v : 'chat');
const downstreamLabelKey = (mode: string) => `models.downstream.${mode}`;
const downstreamHintKey = (mode: string) => `models.downstreamHint.${mode}`;
// 账户是否支持某协议：default 不调用（调用方已跳过）
const supports = (acc: any, proto: string) =>
  proto === 'chat' ? !!acc.chat_endpoint
  : proto === 'responses' ? !!acc.responses_endpoint
  : !!acc.messages_endpoint;

export default function Models() {
  const { t, i18n } = useTranslation();
  const { availableModels, aliases, aggregateAliases, accounts, isLoading, streaming, fetchModels, streamModels, fetchAliases, fetchAggregateAliases, fetchAccounts, addAlias, deleteAlias, saveAggregateAlias, deleteAggregateAlias, setAggregateActive, testModel } = useModelsStore();
  const safeModels = availableModels || [];
  const safeAccounts = accounts || [];
  const [search, setSearch] = useState('');
  const [activeProvider, setActiveProvider] = useState<string>('');
  const [isModalOpen, setIsModalOpen] = useState(false);
  const [aliasForm, setAliasForm] = useState({ alias: '', target: '', provider: '', selectedAccountIds: [] as number[], preferredAccountId: null as number | null, downstreamMode: 'chat' as string });
  // 提交时若强制模式选中了不支持的账户，提示并高亮
  const [aliasModeNotice, setAliasModeNotice] = useState<string | null>(null);
  // 测速/健康按账户隔离：key = accountId:modelId（避免同名模型互串）
  const healthKey = (accountId: number | null | undefined, modelId: string) => `${accountId ?? 'na'}:${modelId}`;
  const modelAccountId = (owned_by: string, modelId: string): number | null => {
    const m = safeModels.find(x => x.owned_by === owned_by && x.id === modelId);
    if (!m) return null;
    const acc = safeAccounts.find(a => a.alias === owned_by);
    return acc ? Number(acc.id) : null;
  };
  // 别名下拉使用复合值，避免同名模型回显串台：value = "owned_by:modelId"
  const optionValue = (owned_by: string, id: string) => `${owned_by}:${id}`;
  const parseOptionValue = (v: string): { owner: string; id: string } => {
    const idx = v.indexOf(':');
    if (idx === -1) return { owner: '', id: v };
    return { owner: v.slice(0, idx), id: v.slice(idx + 1) };
  };

  const [testResults, setTestResults] = useState<Record<string, { success: boolean; latency?: number; error?: string; loading?: boolean; lastChecked?: string; limitsCache?: any; limitsUpdatedAt?: string }>>({});
  const [queueStatus, setQueueStatus] = useState<{ isRunning: boolean; current: number; total: number; progress: number }>({ isRunning: false, current: 0, total: 0, progress: 0 });
  const { startTestQueue, fetchTestQueueStatus } = useModelsStore();
  const [testAllConfirm, setTestAllConfirm] = useState(false);
  const [aliasToDelete, setAliasToDelete] = useState<{id: number, name: string} | null>(null);
  const [aggregateToDelete, setAggregateToDelete] = useState<{id: number, name: string} | null>(null);
  const [overwriteConfirm, setOverwriteConfirm] = useState<{ kind: 'ordinary'|'aggregate', alias: string, pending: any } | null>(null);
  const [editingAliasId, setEditingAliasId] = useState<number | null>(null);
  const [isAggregateModalOpen, setIsAggregateModalOpen] = useState(false);
  const [editingAggregateId, setEditingAggregateId] = useState<number | null>(null);
  const [aggregateForm, setAggregateForm] = useState<{ alias: string; downstreamMode: string; candidates: { account_id: number | ''; model: string }[] }>({ alias: '', downstreamMode: 'chat', candidates: [{ account_id: '', model: '' }] });

  const handleTest = async (modelId: string, providerId: string, accountId?: number) => {
    let resolvedAccountId = accountId ?? null;
    if (resolvedAccountId == null) {
      resolvedAccountId = modelAccountId(providerId, modelId);
    }
    const key = healthKey(resolvedAccountId, modelId);
    setTestResults(prev => ({ ...prev, [key]: { success: false, loading: true } }));
    const result = await testModel(modelId, providerId, resolvedAccountId ?? undefined);
    // @ts-ignore
    setTestResults(prev => ({ ...prev, [key]: { ...result, loading: false } }));
  };

  const fetchHealth = async () => {
    try {
      const res = await apiFetch('/api/models/health');
      if (res.ok) {
        const data: any[] = await res.json();
        setTestResults(prev => {
          const next = { ...prev };
          // 按 (account_id, model) 隔离写入，不再按裸 model 合并
          data.forEach((row: any) => {
            const key = healthKey(row.account_id, row.model);
            if (next[key]?.loading) return;
            next[key] = {
              success: Boolean(row.success),
              latency: row.latency,
              error: row.error,
              lastChecked: row.last_checked,
              limitsCache: row.limits_cache,
              limitsUpdatedAt: row.limits_cache_updated_at
            };
          });
          return next;
        });
      }
    } catch (e) {
      console.error("Failed to fetch models health", e);
    }
  };

  // 1. 初始加载数据：秒开用缓存快照，随后自动开流增量刷新
  useEffect(() => {
    fetchModels().then(() => streamModels(false));
    fetchAliases();
    fetchAggregateAliases();
    fetchAccounts();
    fetchHealth();
    fetchTestQueueStatus().then(setQueueStatus);
  }, []);

  // 2. 智能轮询：仅在队列运行时开启定时器
  useEffect(() => {
    if (!queueStatus.isRunning) return;

    const timer = setInterval(async () => {
      const status = await fetchTestQueueStatus();
      setQueueStatus(status);

      // 如果还在跑，顺便刷新健康状态
      if (status.isRunning) {
        fetchHealth();
      }
    }, 2000);

    return () => clearInterval(timer);
  }, [queueStatus.isRunning]);

  const providers = useMemo(() => {
    const p = Array.from(new Set((safeModels).map(m => m.owned_by)));
    return p;
  }, [safeModels]);

  // 当模型列表加载完成后，如果还没选厂商，默认选第一个
  useEffect(() => {
    if (providers.length > 0 && !activeProvider) {
      setActiveProvider(providers[0]);
    }
  }, [providers, activeProvider]);

  const filteredModels = useMemo(() => {
    return (safeModels).filter(m => {
      const matchSearch = (m.id ?? '').toLowerCase().includes(search.toLowerCase()) ||
                          (m.owned_by ?? '').toLowerCase().includes(search.toLowerCase());
      const matchProvider = m.owned_by === activeProvider;
      return matchSearch && matchProvider;
    });
  }, [safeModels, search, activeProvider]);

  const handleTestAll = () => {
    if (queueStatus.isRunning) return;
    setTestAllConfirm(true);
  };

  const executeTestAll = async () => {
    // 仅测试已配置别名的模型，并携带 accountId 以隔离同名模型
    const modelsToTest = aliases.map(a => {
      let accountId: number | undefined = undefined;
      if (a.account_ids) {
        try { const ids: number[] = JSON.parse(a.account_ids); if (ids.length) accountId = Number(ids[0]); } catch {}
      }
      if (accountId == null && a.preferred_account_id != null) accountId = Number(a.preferred_account_id);
      if (accountId == null && a.provider_id) {
        const acc = safeAccounts.find(x => x.alias === a.provider_id);
        if (acc) accountId = Number(acc.id);
      }
      return { model: a.target_model, providerId: a.provider_id || '', ...(accountId != null ? { accountId } : {}) };
    }).filter(m => m.model);

    if (modelsToTest.length === 0) {
      setTestAllConfirm(false);
      return;
    }

    await startTestQueue(modelsToTest);

    // 立即刷新状态
    const status = await fetchTestQueueStatus();
    setQueueStatus(status);
    setTestAllConfirm(false);
  };
  const handleAddAlias = async (e: React.FormEvent) => {
    e.preventDefault();
    try {
      // If editing and alias name changed, delete old alias first
      if (editingAliasId !== null) {
        const originalAlias = aliases.find(a => a.id === editingAliasId);
        if (originalAlias && originalAlias.alias !== aliasForm.alias) {
          await deleteAlias(editingAliasId);
        }
      }
      // 强制模式（非 default）下，选中账户必须支持该协议，否则 409 提示
      if (aliasForm.downstreamMode !== 'default') {
        const unsupported = aliasForm.selectedAccountIds.filter(id => {
          const acc = safeAccounts.find(a => a.id === id);
          return acc ? !supports(acc, aliasForm.downstreamMode) : true;
        });
        if (unsupported.length > 0) {
          setAliasModeNotice(`有 ${unsupported.length} 个已选账户不支持「${aliasForm.downstreamMode}」下游模式，请取消勾选或更换模式。`);
          return;
        }
      }
      await addAlias(
        aliasForm.alias,
        aliasForm.target,
        aliasForm.provider || undefined,
        aliasForm.selectedAccountIds.length > 0 ? aliasForm.selectedAccountIds : undefined,
        aliasForm.preferredAccountId ?? undefined,
        undefined,
        aliasForm.downstreamMode || 'default'
      );
      setIsModalOpen(false);
      setEditingAliasId(null);
      setAliasForm({ alias: '', target: '', provider: '', selectedAccountIds: [], preferredAccountId: null, downstreamMode: 'default' });
    } catch (err: any) {
      if (err?.status === 409 && err?.conflict === 'aggregate') {
        setOverwriteConfirm({ kind: 'aggregate', alias: aliasForm.alias, pending: { target: aliasForm.target, provider: aliasForm.provider, selectedAccountIds: aliasForm.selectedAccountIds, preferredAccountId: aliasForm.preferredAccountId, downstreamMode: aliasForm.downstreamMode } });
        return;
      }
      // 服务端强制模式校验返回 409（alias_protocol_unsupported）：高亮提示
      if (err?.status === 409) {
        setAliasModeNotice(err?.message || `所选账户不支持「${aliasForm.downstreamMode}」下游模式，请取消勾选或更换模式。`);
        return;
      }
      console.error(err);
    }
  };

  const closeAliasModal = () => {
    setIsModalOpen(false);
    setEditingAliasId(null);
    setAliasModeNotice(null);
    setAliasForm({ alias: '', target: '', provider: '', selectedAccountIds: [], preferredAccountId: null, downstreamMode: 'default' });
  };

  const openAggregateModal = (agg?: any) => {
    if (agg) {
      setEditingAggregateId(agg.id);
      setAggregateForm({ alias: agg.alias, downstreamMode: normalizeMode((agg as any).upstream_api), candidates: agg.candidates.map((c: any) => ({ account_id: c.account_id, model: c.model })) });
    } else {
      setEditingAggregateId(null);
      setAggregateForm({ alias: '', downstreamMode: 'default', candidates: [{ account_id: '', model: '' }] });
    }
    setIsAggregateModalOpen(true);
  };

  const closeAggregateModal = () => {
    setIsAggregateModalOpen(false);
    setEditingAggregateId(null);
    setAggregateForm({ alias: '', downstreamMode: 'default', candidates: [{ account_id: '', model: '' }] });
  };

  const handleSaveAggregate = async (e: React.FormEvent) => {
    e.preventDefault();
    const candidates = aggregateForm.candidates
      .filter(c => c.account_id !== '' && c.model.trim() !== '')
      .map(c => ({ account_id: Number(c.account_id), model: c.model.trim() }));
    if (!aggregateForm.alias.trim() || candidates.length === 0) return;
    // If editing and alias changed, delete old first
    if (editingAggregateId !== null) {
      const orig = aggregateAliases.find(a => a.id === editingAggregateId);
      if (orig && orig.alias !== aggregateForm.alias.trim()) {
        await deleteAggregateAlias(editingAggregateId);
      }
    }
    try {
      await saveAggregateAlias(aggregateForm.alias.trim(), candidates, undefined, undefined, aggregateForm.downstreamMode || 'default');
      closeAggregateModal();
    } catch (err: any) {
      if (err?.status === 409 && err?.conflict === 'ordinary') {
        setOverwriteConfirm({ kind: 'ordinary', alias: aggregateForm.alias.trim(), pending: candidates });
        return;
      }
      console.error(err);
    }
  };

  return (
    <div className="space-y-8 animate-fadeIn">
      {/* Header */}
      <div className="flex flex-col sm:flex-row sm:items-center justify-between gap-4">
        <div className="flex items-start gap-3">
          <div className="p-2 bg-primary/10 text-primary rounded-lg mt-1.5">
            <Box size={24} />
          </div>
          <div>
            <h1 className="text-2xl font-semibold tracking-tight">{t('common.models')}</h1>
            <p className="text-sm text-muted-foreground">{t('models.subtitle')}</p>
          </div>
        </div>
        <div className="flex items-center gap-2">
           <Button
             variant="outline"
             size="sm"
             onClick={handleTestAll}
             disabled={queueStatus.isRunning || filteredModels.length === 0}
             className="bg-warning/10 text-warning hover:bg-warning/20 border-0"
             title={t('models.testAllDesc')}
           >
             <Zap size={16} className={cn(queueStatus.isRunning && "animate-pulse")} />
             {queueStatus.isRunning ? t('models.testingQueue', { current: queueStatus.current, total: queueStatus.total }) : t('models.testAll')}
           </Button>
           <Button
             variant="outline"
             size="sm"
             onClick={() => { streamModels(true); fetchHealth(); }}
             title={t('models.actions.refresh')}
           >
             <RefreshCcw size={16} className={cn((isLoading || streaming) && "animate-spin")} />
             {t('models.actions.refresh')}
           </Button>
        </div>
      </div>

      {/* Aliases Section (Condensed) */}
      <div className="space-y-4">
        <div className="flex items-center justify-between gap-2 px-1">
          <h2 className="text-xs font-semibold text-muted-foreground uppercase tracking-[0.2em] flex items-center gap-2">
            <Zap size={14} className="text-primary" />
            {'别名'}
          </h2>
          <Button
            variant="outline"
            size="sm"
            onClick={() => { setEditingAliasId(null); setAliasForm({ alias: '', target: '', provider: '', selectedAccountIds: [], preferredAccountId: null, downstreamMode: 'default' }); setIsModalOpen(true); }}
          >
            <Plus size={16} />
            {t('models.createAlias')}
          </Button>
        </div>
        {aliases.length > 0 && (
        <div className="grid grid-cols-1 sm:grid-cols-2 md:grid-cols-3 gap-3">
              {aliases.map(a => {
                let aliasAccountId: number | null = null;
                if (a.account_ids) { try { const ids: number[] = JSON.parse(a.account_ids); if (ids.length) aliasAccountId = Number(ids[0]); } catch {} }
                if (aliasAccountId == null && a.preferred_account_id != null) aliasAccountId = Number(a.preferred_account_id);
                if (aliasAccountId == null && a.provider_id) { const acc = safeAccounts.find(x => x.alias === a.provider_id); if (acc) aliasAccountId = Number(acc.id); }
                const result = testResults[aliasAccountId != null ? healthKey(aliasAccountId, a.target_model) : a.target_model] ?? testResults[a.target_model];
                return (
                  <div
                    key={a.id}
                    onClick={() => {
                      let selectedIds: number[] = [];
                      if (a.account_ids) {
                        try { selectedIds = JSON.parse(a.account_ids); } catch {}
                      }
                      setAliasForm({
                        alias: a.alias,
                        target: a.target_model,
                        provider: a.provider_id || '',
                        selectedAccountIds: selectedIds,
                        preferredAccountId: a.preferred_account_id,
                        downstreamMode: normalizeMode((a as any).upstream_api),
                      });
                      setEditingAliasId(a.id);
                      setIsModalOpen(true);
                    }}
                    className="p-2.5 bg-card border border-border rounded-xl flex items-center justify-between group hover:border-primary/30 transition-all cursor-pointer"
                  >
                    <div className="flex items-center gap-2 min-w-0">
                        <div className="flex items-center gap-1 group/alias">
                           <span className="px-2 py-0.5 bg-primary/10 text-primary rounded text-xs font-bold uppercase truncate shadow-sm border border-primary/5">
                             {a.alias}
                           </span>
                           <CopyButton value={a.alias} size={10} className="p-1 opacity-0 group-hover/alias:opacity-100 transition-opacity" />
                        </div>
                        <ArrowRight size={10} className="text-muted-foreground opacity-30 shrink-0" />
                        <div className="flex items-center gap-2 min-w-0">
                          <div className="text-xs font-bold truncate text-muted-foreground">{a.target_model}</div>
                          {result && (
                            <div className="flex items-center gap-1.5 shrink-0">
                              <div className={cn(
                                "w-1.5 h-1.5 rounded-full",
                                result.success ? "bg-success" : "bg-destructive"
                              )} />
                              {result.latency != null && (
                                <span className="text-xs font-bold text-muted-foreground/50">{(result.latency / 1000).toFixed(1)}s</span>
                              )}
                            </div>
                          )}
                        </div>
                    </div>
                    <Button
                      variant="ghost"
                      size="icon"
                      onClick={(e) => { e.stopPropagation(); setAliasToDelete({ id: a.id, name: a.alias }); }}
                      className="h-6 w-6 text-muted-foreground hover:text-destructive transition-all"
                    >
                      <Trash2 size={12} />
                    </Button>
                  </div>
                );
              })}
        </div>
        )}
      </div>

      {/* Aggregate Aliases Section */}
      <div className="space-y-4">
        <div className="flex items-center justify-between gap-2 px-1">
          <h2 className="text-xs font-semibold text-muted-foreground uppercase tracking-[0.2em] flex items-center gap-2">
            <Layers size={14} className="text-primary" />
            {t('models.aggregateAlias')}
          </h2>
          <Button
            variant="outline"
            size="sm"
            onClick={() => openAggregateModal()}
          >
            <Layers size={16} />
            {t('models.aggregateAlias')}
          </Button>
        </div>
        {aggregateAliases.length > 0 && (
        <div className="grid grid-cols-1 sm:grid-cols-2 md:grid-cols-3 gap-3">
              {aggregateAliases.map(agg => {
                const isActive = (idx: number) => agg.active === idx;
                const statusDot = (idx: number) => {
                  const s = agg.last_status?.[idx];
                  if (s === true) return "bg-success";
                  if (s === false) return "bg-destructive";
                  return "bg-muted-foreground/30";
                };
                const pendingNote = agg.pending_target != null ? ` ⏳待切换到 #${agg.pending_target + 1} (${agg.confirm_count}/3)` : "";
                return (
                  <div
                    key={agg.id}
                    onClick={() => openAggregateModal(agg)}
                    className="p-2.5 bg-card border border-border rounded-xl flex flex-col gap-2 group hover:border-primary/30 transition-all cursor-pointer min-w-0"
                  >
                    <div className="flex items-center justify-between gap-2 min-w-0">
                      <div className="flex items-center gap-1.5 min-w-0">
                        <span className="px-2 py-0.5 bg-primary/10 text-primary rounded text-xs font-bold uppercase truncate shadow-sm border border-primary/5 shrink-0">{agg.alias}</span>
                        <CopyButton value={agg.alias} size={10} className="p-1 opacity-0 group-hover:opacity-100 transition-opacity shrink-0" />
                        {pendingNote && <span className="text-xs text-warning font-bold truncate">{pendingNote}</span>}
                      </div>
                      <Button variant="ghost" size="icon" onClick={(e) => { e.stopPropagation(); setAggregateToDelete({ id: agg.id, name: agg.alias }); }} className="h-6 w-6 shrink-0 text-muted-foreground hover:text-destructive transition-all"><Trash2 size={12} /></Button>
                    </div>
                    <div className="space-y-1">
                      {agg.candidates.map((c: any, idx: number) => {
                        const acc = safeAccounts.find(a => a.id === c.account_id);
                        return (
                          <div key={idx} className={cn("flex flex-col gap-1 text-xs px-2 py-1.5 rounded", isActive(idx) ? "bg-primary/10 border border-primary/20" : "bg-muted/30")}>
                            <div className="flex items-center gap-2 min-w-0">
                              <span className="font-bold text-muted-foreground w-4 shrink-0">#{idx+1}</span>
                              <div className={cn("w-2 h-2 rounded-full shrink-0", statusDot(idx))} title={String(agg.last_status?.[idx] ?? "unknown")} />
                              <span className="font-bold truncate">{acc ? acc.alias : `account#${c.account_id}`}</span>
                              {isActive(idx) ? <span className="ml-auto shrink-0 text-primary font-bold text-[10px] px-1.5 py-0.5 rounded bg-primary/10 border border-primary/20">活跃</span> : <button onClick={(e) => { e.stopPropagation(); setAggregateActive(agg.id, idx).catch(()=>{}); }} className="ml-auto shrink-0 text-[10px] px-1.5 py-0.5 rounded border border-border bg-muted/50 hover:bg-primary/10 hover:text-primary transition-colors">设为活跃</button>}
                            </div>
                            <div className="pl-6 truncate text-muted-foreground">{c.model}</div>
                          </div>
                        );
                      })}
                    </div>
                  </div>
                );
              })}
        </div>
        )}
      </div>

      {/* Filters & Tabs */}
      <div className="space-y-4">
        <div className="flex items-center justify-between gap-4">
           <Tabs value={activeProvider} onValueChange={setActiveProvider} className="overflow-x-auto">
              <TabsList className="bg-muted/50 border border-border/50">
                {providers.map(p => (
                  <TabsTrigger key={p} value={p} className="text-xs font-bold capitalize">
                    {p}
                  </TabsTrigger>
                ))}
                {providers.length === 0 && <span className="px-4 py-1.5 text-xs text-muted-foreground italic">No providers</span>}
                {providers.length > 0 && <span className="shrink-0 w-1.5" aria-hidden />}
              </TabsList>
           </Tabs>

           <div className="relative flex-1 min-w-[110px] max-w-[170px]">
              {search === '' && (
                <Search className="absolute left-3 top-1/2 -translate-y-1/2 text-muted-foreground z-10" size={14} />
              )}
              <Input
                type="text"
                placeholder={t('models.filter.searchPlaceholder')}
                value={search}
                onChange={e => setSearch(e.target.value)}
                className={search ? 'pl-3' : 'pl-9'}
              />
           </div>
        </div>
      </div>

      {/* Models Grid — key by owned_by:modelId so 同名模型在不同账户各有一张卡 */}
      <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 xl:grid-cols-4 2xl:grid-cols-5 gap-4">
        {filteredModels.map((model) => {
          const isPlaceholder = model.id?.endsWith('-models-unavailable');
          const cardAccountId = modelAccountId(model.owned_by, model.id);
          const cardKey = healthKey(cardAccountId, model.id);
          const cardResult = (!isPlaceholder ? (testResults[cardKey] ?? testResults[model.id]) : undefined);
          return (
          <div key={`${model.owned_by}:${model.id}`} className={cn("p-4 rounded-xl border bg-card hover:border-primary/40 transition-all group flex flex-col justify-between min-h-[160px]", isPlaceholder ? "border-dashed border-warning/30 bg-warning/5" : "border-border")}>
            <div className="space-y-1">
              <div className="flex items-center justify-between">
                <span className="text-xs font-bold text-primary uppercase tracking-widest">{model.owned_by}</span>
                <div className="flex items-center gap-1.5">
                   {!isPlaceholder && cardResult?.loading ? (
                     <RefreshCcw size={10} className="animate-spin text-muted-foreground" />
                   ) : !isPlaceholder && cardResult ? (
                     <div className={cn(
                       "w-1.5 h-1.5 rounded-full",
                       cardResult?.success ? "bg-success shadow-[0_0_8px_rgba(34,197,94,0.6)]" : "bg-destructive shadow-[0_0_8px_rgba(239,68,68,0.6)]"
                     )} title={cardResult?.error} />
                   ) : null}
                   <LayoutGrid size={12} className="text-muted-foreground/30" />
                </div>
              </div>
              <div className="flex items-start justify-between gap-2">
                <h3 className="font-semibold text-sm tracking-tight line-clamp-2 leading-snug">{model.name || model.id}</h3>
                <CopyButton 
                  value={model.id} 
                  size={12} 
                  className="mt-0.5 opacity-40 hover:opacity-100 transition-opacity" 
                  title={t('models.actions.copyName')} 
                />
              </div>
              <div className="flex items-center gap-2">
                {model.context_length != null && model.context_length > 0 && (
                  <span
                    className="text-[10px] font-bold text-muted-foreground/60 bg-muted/50 border border-border/50 rounded px-1.5 py-0.5"
                    title={`${t('models.contextLength')}: ${model.context_length.toLocaleString()}`}
                  >
                    {formatContextLength(model.context_length)}
                  </span>
                )}
                {cardResult?.latency != null && (
                  <span className="text-xs text-success font-bold">{(cardResult!.latency! / 1000).toFixed(1)}s</span>
                )}
                {cardResult?.lastChecked && (
                  <span className="text-xs text-muted-foreground/60 font-medium">
                    {parseServerDate(cardResult!.lastChecked!).toLocaleString(i18n.language, {
                      month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit'
                    })}
                  </span>
                )}
              </div>
              {cardResult?.error && (
                <p className="text-xs text-destructive font-medium line-clamp-1 opacity-80" title={cardResult?.error}>{cardResult?.error}</p>
              )}
              {/* 限额进度条：只有厂商返回了 ratelimit 数据才显示 */}
              {(() => {
                const limits = cardResult?.limitsCache;
                const limitsUpdatedAt = cardResult?.limitsUpdatedAt;
                if (!limits) return null;
                const remaining = parseInt(limits['x-ratelimit-remaining-tokens'] ?? limits['x-quota-remaining'] ?? -1);
                const total = parseInt(limits['x-ratelimit-limit-tokens'] ?? limits['x-quota-total'] ?? -1);
                if (remaining < 0 || total <= 0) return null;
                const pct = Math.max(0, Math.min(100, (remaining / total) * 100));
                const color = pct > 50 ? 'bg-success' : pct > 15 ? 'bg-warning' : 'bg-destructive';
                return (
                  <div className="mt-1.5 space-y-0.5">
                    <div className="flex justify-between text-xs text-muted-foreground">
                      <span>Tokens</span>
                      <span>{remaining.toLocaleString()} / {total.toLocaleString()}</span>
                    </div>
                    <div className="h-1 w-full rounded-full bg-muted/50 overflow-hidden">
                      <div
                        className={cn("h-full rounded-full transition-all duration-700", color)}
                        style={{ width: `${pct}%` }}
                      />
                    </div>
                    {limitsUpdatedAt && (
                      <div className="text-xs text-muted-foreground/40 text-right">
                        更新于 {parseServerDate(limitsUpdatedAt).toLocaleString(i18n.language, {
                          month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit'
                        })}
                      </div>
                    )}
                  </div>
                );
              })()}
            </div>
            
            <div className="pt-3 mt-3 border-t border-border/40 flex items-center justify-between text-xs font-bold text-muted-foreground uppercase tracking-tighter">
               {isPlaceholder ? (
                 <p className="text-xs text-warning font-normal normal-case truncate" title={(model as any).error}>{(model as any).error || t('models.apiUnavailable')}</p>
               ) : (
                 <button
                   onClick={() => handleTest(model.id, model.owned_by, cardAccountId ?? undefined)}
                   disabled={cardResult?.loading || queueStatus.isRunning}
                   className="flex items-center gap-1 hover:text-foreground transition-colors disabled:opacity-50"
                 >
                   <Zap size={12} className={cn(cardResult?.success && "text-warning")} />
                   {cardResult?.loading ? t('models.testing') : t('models.testBtn')}
                 </button>
               )}
               {!isPlaceholder && (
                 <button
                   onClick={() => {
                      setEditingAliasId(null);
                      const matchingOwners = [...new Set(safeModels.filter(x => x.id === model.id).map(x => x.owned_by))];
                      const matchingIds = safeAccounts.filter(a => matchingOwners.includes(a.alias) && a.is_active === 1).map(a => a.id);
                      setAliasForm({ alias: '', target: model.id, provider: model.owned_by, selectedAccountIds: matchingIds, preferredAccountId: null, downstreamMode: 'default' });
                      setIsModalOpen(true);
                   }}
                   className="flex items-center gap-1 text-primary hover:opacity-80 transition-opacity"
                 >
                   {t('models.actions.assign')}
                   <ChevronRight size={12} />
                 </button>
               )}
            </div>
          </div>
        )})}
      </div>

      {/* Empty State */}
      {filteredModels.length === 0 && !isLoading && (
        <div className="py-20 text-center border-2 border-dashed border-border rounded-3xl">
           <p className="text-sm text-muted-foreground font-medium italic">
             {providers.length === 0 ? t('models.noAccountsConnected') : t('models.noModelsFound')}
           </p>
        </div>
      )}

      {/* Add Alias Modal */}
      <Dialog isOpen={isModalOpen} onClose={closeAliasModal} title={editingAliasId !== null ? t('models.editAlias') : t('models.createAlias')} size="lg">
        <form onSubmit={handleAddAlias} className="space-y-4">
          <div className="space-y-1.5">
            <label className="text-xs font-bold text-muted-foreground uppercase">{t('models.aliasName')}</label>
            <Input
              type="text" required value={aliasForm.alias}
              onChange={e => setAliasForm({...aliasForm, alias: e.target.value})}
              placeholder={t('models.aliasPlaceholder')}
            />
          </div>
          <div className="space-y-1.5">
            <label className="text-xs font-bold text-muted-foreground uppercase">{t('models.targetModel')}</label>
            <select
              value={aliasForm.target ? optionValue(aliasForm.provider || safeModels.find(m => m.id === aliasForm.target)?.owned_by || '', aliasForm.target) : ''}
              onChange={e => {
                const parsed = parseOptionValue(e.target.value);
                const accts = safeAccounts;
                if (!parsed.id) {
                  setAliasForm({ ...aliasForm, target: '', provider: '', selectedAccountIds: [] });
                  return;
                }
                // 按模型 id 匹配所有拥有该模型的账户，不只所选 owner，避免同名模型在多账户间被隐藏
                const matchingAccounts = accts.filter(a => a.is_active === 1 && safeModels.some(m => m.id === parsed.id && m.owned_by === a.alias));
                setAliasForm({
                  ...aliasForm,
                  target: parsed.id,
                  provider: parsed.owner,
                  selectedAccountIds: matchingAccounts.map(a => a.id),
                });
              }}
              className="w-full h-10 px-3 py-2 rounded-md border border-input bg-background text-sm ring-offset-background focus:outline-none focus:ring-2 focus:ring-ring focus:ring-offset-2"
            >
              <option value="">{t('common.default')}</option>
              {safeModels.map(mod => (
                <option key={`${mod.owned_by}:${mod.id}`} value={optionValue(mod.owned_by, mod.id)}>[{mod.owned_by}] {mod.id}</option>
              ))}
            </select>
          </div>
          {(() => {
            const accts = safeAccounts;
            const matchingAccounts = aliasForm.target
              ? accts.filter(a => a.is_active === 1 && safeModels.some(m => m.id === aliasForm.target && m.owned_by === a.alias))
              : [];
            const otherAccounts = accts.filter(a => !matchingAccounts.some(m => m.id === a.id) && a.is_active === 1);
            // Unsupported accounts stay visible (with a per-row warning); the
            // runtime skips them for forced modes.
            const filtered = matchingAccounts;
            if (!aliasForm.target) return null;
            return (
              <div className="space-y-1.5 border-t border-border pt-3">
                <label className="text-xs font-bold text-muted-foreground uppercase">
                  {t('models.bindAccounts')}
                  {filtered.length > 0 && (
                    <span className="ml-1 text-primary font-normal">({filtered.length})</span>
                  )}
                </label>
                <p className="text-xs text-muted-foreground">{t('models.bindAccountsHint')}</p>
                {aliasForm.target && filtered.length === 0 && (
                  <p className="text-xs text-warning">{t('models.noAccountsForModel')}</p>
                )}
                {aliasForm.target && otherAccounts.length > 0 && (
                  <p className="text-xs text-muted-foreground/60">{t('models.otherAccountsHidden', { count: otherAccounts.length })}</p>
                )}
                {filtered.length > 0 && (
                  <div className="flex items-center gap-2">
                    <button
                      type="button"
                      onClick={() => setAliasForm({...aliasForm, selectedAccountIds: filtered.map(a => a.id)})}
                      className="text-xs font-bold text-primary hover:underline"
                    >{t('models.selectAll')}</button>
                    <button
                      type="button"
                      onClick={() => setAliasForm({...aliasForm, selectedAccountIds: []})}
                      className="text-xs font-bold text-muted-foreground hover:underline"
                    >{t('models.deselectAll')}</button>
                  </div>
                )}
                <div className="max-h-32 overflow-y-auto space-y-1 border border-border rounded-lg p-2 bg-muted/30">
                  {filtered.map(a => {
                    const unsupported = aliasForm.downstreamMode !== 'default' && !supports(a, aliasForm.downstreamMode);
                    return (
                <label key={a.id} className={cn("flex items-center gap-2 px-2 py-1 rounded cursor-pointer", unsupported ? "bg-destructive/5 ring-1 ring-destructive/30" : "hover:bg-muted/50")}>
                  <input
                    type="checkbox"
                    checked={aliasForm.selectedAccountIds.includes(a.id)}
                    onChange={e => {
                      if (e.target.checked) {
                        setAliasForm({...aliasForm, selectedAccountIds: [...aliasForm.selectedAccountIds, a.id]});
                      } else {
                        setAliasForm({...aliasForm, selectedAccountIds: aliasForm.selectedAccountIds.filter(id => id !== a.id)});
                      }
                    }}
                    className="w-3.5 h-3.5 rounded accent-primary"
                  />
                  <span className="text-xs">[{a.provider_id}] {a.alias}</span>
                  {unsupported && (
                    <span className="text-[10px] text-destructive font-medium">{t('models.downstreamUnsupportedBadge', { mode: t(downstreamLabelKey(aliasForm.downstreamMode)) })}</span>
                  )}
                </label>
              )})}
                  {filtered.length === 0 && (
                    <p className="text-xs text-muted-foreground p-2">{t('accounts.noAccounts')}</p>
                  )}
                </div>
                {aliasForm.selectedAccountIds.length > 0 && (
                  <div className="space-y-1.5 border-t border-border pt-3">
                    <label className="text-xs font-bold text-muted-foreground uppercase">
                      {t('models.preferredAccount')}
                    </label>
                    <p className="text-xs text-muted-foreground">{t('models.preferredAccountHint')}</p>
                    <select
                      value={aliasForm.preferredAccountId ?? ''}
                      onChange={e => setAliasForm({...aliasForm, preferredAccountId: e.target.value ? Number(e.target.value) : null})}
                      className="w-full h-10 px-3 py-2 rounded-md border border-input bg-background text-sm ring-offset-background focus:outline-none focus:ring-2 focus:ring-ring focus:ring-offset-2"
                    >
                      <option value="">{t('models.preferredAccountAuto')}</option>
                      {filtered.filter(a => aliasForm.selectedAccountIds.includes(a.id)).map(a => (
                        <option key={a.id} value={a.id}>[{a.provider_id}] {a.alias}</option>
                      ))}
                    </select>
                  </div>
                )}
              </div>
            );
          })()}
          <div className="space-y-1.5 border-t border-border pt-3">
            <label className="text-xs font-bold text-muted-foreground uppercase">{t('models.downstreamMode')}</label>
            <p className="text-xs text-muted-foreground">{t(downstreamHintKey(aliasForm.downstreamMode))}</p>
            <select
              value={aliasForm.downstreamMode}
              onChange={e => {
                const mode = e.target.value;
                // 切换模式时，移除不再支持该协议的已选账户
                const nextSelected = mode === 'default' ? aliasForm.selectedAccountIds
                  : aliasForm.selectedAccountIds.filter(id => {
                      const acc = safeAccounts.find(a => a.id === id);
                      return acc ? supports(acc, mode) : false;
                    });
                setAliasModeNotice(null);
                setAliasForm({ ...aliasForm, downstreamMode: mode, selectedAccountIds: nextSelected });
              }}
              className="w-full h-10 px-3 py-2 rounded-md border border-input bg-background text-sm"
            >
              {DOWNSTREAM_MODES.map(m => (
                <option key={m} value={m}>{t(downstreamLabelKey(m))}</option>
              ))}
            </select>
            {aliasModeNotice && (
              <p className="text-xs text-destructive font-medium bg-destructive/10 border border-destructive/30 rounded px-2 py-1.5">{aliasModeNotice}</p>
            )}
          </div>
          <div className="pt-4 flex gap-3">
             <Button type="button" variant="outline" onClick={closeAliasModal} className="flex-1">{t('common.cancel')}</Button>
             <Button type="submit" className="flex-1">
               <Save size={16} /> {editingAliasId !== null ? t('common.update') : t('common.save')}
             </Button>
          </div>
        </form>
      </Dialog>

      {/* Aggregate Alias Modal */}
      <Dialog isOpen={isAggregateModalOpen} onClose={closeAggregateModal} title={editingAggregateId !== null ? t('models.editAlias') : t('models.aggregateAlias')}>
        <form onSubmit={handleSaveAggregate} className="space-y-4">
          <div className="space-y-1.5">
            <label className="text-xs font-bold text-muted-foreground uppercase">{t('models.aliasName')}</label>
            <Input type="text" required value={aggregateForm.alias} onChange={e => setAggregateForm({ ...aggregateForm, alias: e.target.value })} placeholder={t('models.aliasPlaceholder')} />
          </div>
          <div className="space-y-1.5">
            <div className="flex items-center justify-between">
              <label className="text-xs font-bold text-muted-foreground uppercase">候选列表（顶部为默认）</label>
              <Button type="button" variant="outline" size="sm" onClick={() => setAggregateForm({ ...aggregateForm, candidates: [...aggregateForm.candidates, { account_id: '', model: '' }] })}>+ 添加候选</Button>
            </div>
            <div className="space-y-2 max-h-72 overflow-y-auto overflow-x-hidden border border-border rounded-lg p-2 bg-muted/30">
              {aggregateForm.candidates.map((c, idx) => (
                <div key={idx} className="flex items-start gap-2 p-2 bg-card border border-border rounded-lg min-w-0">
                  <span className="text-xs font-bold w-5 shrink-0 pt-2">#{idx+1}</span>
                  <div className="flex-1 flex flex-col gap-1.5 min-w-0">
                    <select
                      value={c.account_id}
                      onChange={e => {
                        const next = [...aggregateForm.candidates];
                        next[idx] = { ...next[idx], account_id: e.target.value ? Number(e.target.value) : '' };
                        setAggregateForm({ ...aggregateForm, candidates: next });
                      }}
                      className="w-full min-w-0 h-8 px-2 rounded-md border border-input bg-background text-sm"
                    >
                      <option value="">选择账户</option>
                      {safeAccounts.filter(a => a.is_active === 1).map(a => (
                        <option key={a.id} value={a.id}>[{a.provider_id}] {a.alias}</option>
                      ))}
                    </select>
                    {(() => {
                      const accAlias = safeAccounts.find(a => a.id === c.account_id)?.alias;
                      const modelsForAccount = c.account_id ? safeModels.filter(m => m.owned_by === accAlias) : [];
                      const hasModels = modelsForAccount.length > 0;
                      const inList = hasModels && modelsForAccount.some(m => m.id === c.model);
                      return (
                        <select
                          value={c.model}
                          disabled={!c.account_id}
                          onChange={e => {
                            const next = [...aggregateForm.candidates];
                            next[idx] = { ...next[idx], model: e.target.value };
                            setAggregateForm({ ...aggregateForm, candidates: next });
                          }}
                          className="w-full min-w-0 h-8 px-2 rounded-md border border-input bg-background text-sm disabled:opacity-50"
                        >
                          <option value="">{!c.account_id ? "请先选择账户" : hasModels ? "选择模型" : "暂无可用模型"}</option>
                          {modelsForAccount.map(m => (
                            <option key={`${m.owned_by}:${m.id}`} value={m.id}>{m.id}</option>
                          ))}
                          {!inList && c.model && <option value={c.model}>{c.model} (不在列表)</option>}
                        </select>
                      );
                    })()}
                    {(() => {
                      if (aggregateForm.downstreamMode === 'default' || c.account_id === '') return null;
                      const acc = safeAccounts.find(a => a.id === c.account_id);
                      if (acc && !supports(acc, aggregateForm.downstreamMode)) {
                        return <p className="text-xs text-destructive font-medium bg-destructive/10 border border-destructive/30 rounded px-2 py-1">{t('models.accountUnsupportedMode', { mode: t(downstreamLabelKey(aggregateForm.downstreamMode)) })}</p>;
                      }
                      return null;
                    })()}
                  </div>
                  <div className="flex flex-col gap-1 shrink-0">
                    <Button type="button" variant="ghost" size="icon" className="h-7 w-7" disabled={idx === 0} onClick={() => {
                      const next = [...aggregateForm.candidates];
                      const tmp = next[idx-1]; next[idx-1] = next[idx]; next[idx] = tmp;
                      setAggregateForm({ ...aggregateForm, candidates: next });
                    }}><ChevronUp size={12} /></Button>
                    <Button type="button" variant="ghost" size="icon" className="h-7 w-7" disabled={idx === aggregateForm.candidates.length - 1} onClick={() => {
                      const next = [...aggregateForm.candidates];
                      const tmp = next[idx+1]; next[idx+1] = next[idx]; next[idx] = tmp;
                      setAggregateForm({ ...aggregateForm, candidates: next });
                    }}><ChevronDown size={12} /></Button>
                    <Button type="button" variant="ghost" size="icon" className="h-7 w-7 text-destructive" disabled={aggregateForm.candidates.length <= 1} onClick={() => {
                      setAggregateForm({ ...aggregateForm, candidates: aggregateForm.candidates.filter((_, i) => i !== idx) });
                    }}><Trash2 size={12} /></Button>
                  </div>
                </div>
              ))}
            </div>

            <p className="text-xs text-muted-foreground">顶部候选为默认模型，拖动排序即改默认/顺序。每行独立账户+模型。</p>
          </div>
          <div className="space-y-1.5 border-t border-border pt-3">
            <label className="text-xs font-bold text-muted-foreground uppercase">{t('models.downstreamMode')}</label>
            <p className="text-xs text-muted-foreground">{t(downstreamHintKey(aggregateForm.downstreamMode))}{aggregateForm.downstreamMode !== 'default' && t('models.downstreamHintValidate')}</p>
            <select
              value={aggregateForm.downstreamMode}
              onChange={e => setAggregateForm({...aggregateForm, downstreamMode: e.target.value})}
              className="w-full h-10 px-3 py-2 rounded-md border border-input bg-background text-sm"
            >
              {DOWNSTREAM_MODES.map(m => (
                <option key={m} value={m}>{t(downstreamLabelKey(m))}</option>
              ))}
            </select>
          </div>
          <div className="pt-4 flex gap-3">
            <Button type="button" variant="outline" onClick={closeAggregateModal} className="flex-1">{t('common.cancel')}</Button>
            <Button type="submit" className="flex-1"><Save size={16} /> {t('common.save')}</Button>
          </div>
        </form>
      </Dialog>

      <ConfirmDialog
        isOpen={!!overwriteConfirm}
        onClose={() => setOverwriteConfirm(null)}
        onConfirm={async () => {
          if (!overwriteConfirm) return;
          if (overwriteConfirm.kind === 'ordinary') {
            await saveAggregateAlias(overwriteConfirm.alias, overwriteConfirm.pending, undefined, true, 'chat');
            closeAggregateModal();
          } else {
            const pd = overwriteConfirm.pending;
            await addAlias(overwriteConfirm.alias, pd.target, pd.provider || undefined, pd.selectedAccountIds?.length ? pd.selectedAccountIds : undefined, pd.preferredAccountId ?? undefined, true, pd.downstreamMode || 'chat');
            setIsModalOpen(false);
            setEditingAliasId(null);
            setAliasForm({ alias: '', target: '', provider: '', selectedAccountIds: [], preferredAccountId: null, downstreamMode: 'default' });
          }
          setOverwriteConfirm(null);
        }}
        title={overwriteConfirm?.kind === 'ordinary' ? '覆盖普通别名？' : '覆盖聚合别名？'}
        description={overwriteConfirm?.kind === 'ordinary' ? `别名 "${overwriteConfirm?.alias}" 已是普通别名，确认用聚合别名覆盖？普通别名将被删除，密钥授权予以保留。` : `别名 "${overwriteConfirm?.alias}" 已是聚合别名，确认用普通别名覆盖？聚合别名将被删除，密钥授权予以保留。`}
        confirmText="确认覆盖"
        variant="warning"
      />

      <ConfirmDialog
        isOpen={testAllConfirm}
        onClose={() => setTestAllConfirm(false)}
        onConfirm={executeTestAll}
        title={t('models.testAllTitle')}
        description={t('models.testAllConfirm', '即将对你已配置别名的模型进行后台顺序拨测。\n\n⚠️注意：测试将真实调用模型接口发出一句简单的问候，每次将消耗约 1 Token 左右的资源。\n如果在执行期间离开此页面，后台测试依然会继续直至完成。是否继续？')}
        confirmText={t('models.testAllStart')}
        variant="warning"
      />

      <ConfirmDialog
        isOpen={!!aliasToDelete}
        onClose={() => setAliasToDelete(null)}
        onConfirm={async () => {
          if (aliasToDelete) {
            await deleteAlias(aliasToDelete.id);
            setAliasToDelete(null);
          }
        }}
        title={t('common.delete')}
        description={t('models.deleteConfirm', { name: aliasToDelete?.name })}
        confirmText={t('common.delete')}
        variant="danger"
      />

      <ConfirmDialog
        isOpen={!!aggregateToDelete}
        onClose={() => setAggregateToDelete(null)}
        onConfirm={async () => {
          if (aggregateToDelete) {
            await deleteAggregateAlias(aggregateToDelete.id);
            setAggregateToDelete(null);
          }
        }}
        title={t('common.delete')}
        description={t('models.deleteConfirm', { name: aggregateToDelete?.name })}
        confirmText={t('common.delete')}
        variant="danger"
      />
    </div>
  );
}
