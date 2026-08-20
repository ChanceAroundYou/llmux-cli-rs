import React, { useEffect, useState, useMemo } from 'react';
import { useModelsStore } from '../stores/models';
import {
  Box,
  Search,
  RefreshCcw,
  ExternalLink,
  ChevronRight,
  Database,
  Plus,
  Save,
  Trash2,
  LayoutGrid,
  Zap,
  ArrowRight,
  Copy,
  PenLine,
  CheckCircle2,
  XCircle,
  Loader2
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

export default function Models() {
  const { t, i18n } = useTranslation();
  const { availableModels, cachedAt, aliases, accounts, isLoading, fetchModels, fetchAliases, fetchAccounts, addAlias, deleteAlias, testModel } = useModelsStore();
  const safeModels = availableModels || [];
  const safeAccounts = accounts || [];
  const [search, setSearch] = useState('');
  const [activeProvider, setActiveProvider] = useState<string>('');
  const [isModalOpen, setIsModalOpen] = useState(false);
  const [aliasForm, setAliasForm] = useState({ alias: '', target: '', provider: '', selectedAccountIds: [] as number[], preferredAccountId: null as number | null });
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
  const [editingAliasId, setEditingAliasId] = useState<number | null>(null);
  const [isCustomModalOpen, setIsCustomModalOpen] = useState(false);
  const [customForm, setCustomForm] = useState({ alias: '', target: '', accountId: '' });
  const [isVerifying, setIsVerifying] = useState(false);
  const [verifyResult, setVerifyResult] = useState<{ success: boolean; error?: string; latency?: number } | null>(null);

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
      const res = await fetch('/api/models/health');
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

  // 1. 初始加载数据
  useEffect(() => {
    fetchModels();
    fetchAliases();
    fetchAccounts();
    fetchHealth();
    // 进入页面时仅检查一次队列状态
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
      await addAlias(
        aliasForm.alias,
        aliasForm.target,
        aliasForm.provider || undefined,
        aliasForm.selectedAccountIds.length > 0 ? aliasForm.selectedAccountIds : undefined,
        aliasForm.preferredAccountId ?? undefined
      );
      setIsModalOpen(false);
      setEditingAliasId(null);
      setAliasForm({ alias: '', target: '', provider: '', selectedAccountIds: [], preferredAccountId: null });
    } catch (err) {
      console.error(err);
    }
  };

  const closeAliasModal = () => {
    setIsModalOpen(false);
    setEditingAliasId(null);
    setAliasForm({ alias: '', target: '', provider: '', selectedAccountIds: [], preferredAccountId: null });
  };

  const handleVerify = async () => {
    if (!customForm.target || !customForm.accountId) return;
    const account = safeAccounts.find(a => a.id === Number(customForm.accountId));
    if (!account) return;

    setIsVerifying(true);
    setVerifyResult(null);
    try {
      const result = await testModel(customForm.target, account.provider_id, account.id);
      setVerifyResult(result);
    } catch (err: any) {
      setVerifyResult({ success: false, error: err.message });
    } finally {
      setIsVerifying(false);
    }
  };

  const handleCustomAddAlias = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!verifyResult?.success) return;
    const account = safeAccounts.find(a => a.id === Number(customForm.accountId));
    try {
      // 使用账户别名作为 provider_id，dispatcher 会通过 alias 回退匹配
      await addAlias(customForm.alias, customForm.target, account?.alias || '');
      setIsCustomModalOpen(false);
      setCustomForm({ alias: '', target: '', accountId: '' });
      setVerifyResult(null);
    } catch (err) {
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
            <p className="text-sm text-muted-foreground">{t('models.subtitle')}{cachedAt ? t('models.cachedAt', { time: new Date(cachedAt * 1000).toLocaleString() }) : ''}</p>
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
             variant="ghost"
             size="icon"
             onClick={() => { fetchModels(true); fetchHealth(); }}
             className="text-muted-foreground"
             title={t('models.actions.refresh')}
           >
             <RefreshCcw size={18} className={cn(isLoading && "animate-spin")} />
           </Button>
           <Button
             size="sm"
             onClick={() => { setEditingAliasId(null); setAliasForm({ alias: '', target: '', provider: '', selectedAccountIds: [], preferredAccountId: null }); setIsModalOpen(true); }}
           >
             <Plus size={16} />
             {t('models.createAlias')}
           </Button>
           <Button
             variant="outline"
             size="sm"
             onClick={() => setIsCustomModalOpen(true)}
           >
             <PenLine size={16} />
             {t('models.customAlias')}
           </Button>
        </div>
      </div>

      {/* Aliases Section (Condensed) */}
      {aliases.length > 0 && (
        <div className="space-y-4">
           <h2 className="text-xs font-semibold text-muted-foreground uppercase tracking-[0.2em] px-1 flex items-center gap-2">
              <Zap size={14} className="text-primary" />
              {t('models.aliasSection')}
           </h2>
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
                      className="h-6 w-6 text-muted-foreground hover:text-destructive opacity-0 group-hover:opacity-100 transition-all"
                    >
                      <Trash2 size={12} />
                    </Button>
                  </div>
                );
              })}
           </div>
        </div>
      )}

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
              </TabsList>
           </Tabs>

           <div className="relative flex-1 max-w-xs">
              <Search className="absolute left-3 top-1/2 -translate-y-1/2 text-muted-foreground z-10" size={14} />
              <Input
                type="text"
                placeholder={t('models.filter.searchPlaceholder')}
                value={search}
                onChange={e => setSearch(e.target.value)}
                className="pl-9"
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
                      setAliasForm({ alias: '', target: model.id, provider: model.owned_by, selectedAccountIds: matchingIds, preferredAccountId: null });
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
      <Dialog isOpen={isModalOpen} onClose={closeAliasModal} title={editingAliasId !== null ? t('models.editAlias') : t('models.createAlias')}>
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
                const matchingAccounts = accts.filter(a => a.alias === parsed.owner && a.is_active === 1);
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
            const matchingAccounts = aliasForm.target && aliasForm.provider
              ? accts.filter(a => a.alias === aliasForm.provider && a.is_active === 1)
              : [];
            const otherAccounts = accts.filter(a => !matchingAccounts.some(m => m.id === a.id) && a.is_active === 1);
            if (!aliasForm.target) return null;
            return (
              <div className="space-y-1.5 border-t border-border pt-3">
                <label className="text-xs font-bold text-muted-foreground uppercase">
                  {t('models.bindAccounts')}
                  {matchingAccounts.length > 0 && (
                    <span className="ml-1 text-primary font-normal">({matchingAccounts.length})</span>
                  )}
                </label>
                <p className="text-xs text-muted-foreground">{t('models.bindAccountsHint')}</p>
                {aliasForm.target && matchingAccounts.length === 0 && (
                  <p className="text-xs text-warning">{t('models.noAccountsForModel')}</p>
                )}
                {aliasForm.target && otherAccounts.length > 0 && (
                  <p className="text-xs text-muted-foreground/60">{t('models.otherAccountsHidden', { count: otherAccounts.length })}</p>
                )}
                {matchingAccounts.length > 0 && (
                  <div className="flex items-center gap-2">
                    <button
                      type="button"
                      onClick={() => setAliasForm({...aliasForm, selectedAccountIds: matchingAccounts.map(a => a.id)})}
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
                  {matchingAccounts.map(a => (
                <label key={a.id} className="flex items-center gap-2 px-2 py-1 hover:bg-muted/50 rounded cursor-pointer">
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
                </label>
              ))}
                  {matchingAccounts.length === 0 && (
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
                      {matchingAccounts.filter(a => aliasForm.selectedAccountIds.includes(a.id)).map(a => (
                        <option key={a.id} value={a.id}>[{a.provider_id}] {a.alias}</option>
                      ))}
                    </select>
                  </div>
                )}
              </div>
            );
          })()}
          <div className="pt-4 flex gap-3">
             <Button type="button" variant="outline" onClick={closeAliasModal} className="flex-1">{t('common.cancel')}</Button>
             <Button type="submit" className="flex-1">
               <Save size={16} /> {editingAliasId !== null ? t('common.update') : t('common.save')}
             </Button>
          </div>
        </form>
      </Dialog>

      {/* Custom Alias Modal */}
      <Dialog isOpen={isCustomModalOpen} onClose={() => { setIsCustomModalOpen(false); setVerifyResult(null); }} title={t('models.customAliasTitle')}>
        <form onSubmit={handleCustomAddAlias} className="space-y-4">
          <div className="space-y-1.5">
            <label className="text-xs font-bold text-muted-foreground uppercase">{t('models.selectAccount')}</label>
            <select
              value={customForm.accountId}
              onChange={e => {
                setCustomForm({ ...customForm, accountId: e.target.value, target: '' });
                setVerifyResult(null);
              }}
              className="w-full h-10 px-3 py-2 rounded-md border border-input bg-background text-sm ring-offset-background focus:outline-none focus:ring-2 focus:ring-ring focus:ring-offset-2"
            >
              <option value="">{t('models.selectAccountPlaceholder')}</option>
              {safeAccounts.filter(a => a.is_active === 1).map(a => (
                <option key={a.id} value={a.id}>[{a.provider_id}] {a.alias}</option>
              ))}
            </select>
          </div>
          <div className="space-y-1.5">
            <label className="text-xs font-bold text-muted-foreground uppercase">{t('models.aliasName')}</label>
            <Input
              type="text" required value={customForm.alias}
              onChange={e => setCustomForm({ ...customForm, alias: e.target.value })}
              placeholder={t('models.aliasPlaceholder')}
            />
          </div>
          <div className="space-y-1.5">
            <label className="text-xs font-bold text-muted-foreground uppercase">{t('models.manualModel')}</label>
            <Input
              type="text" required value={customForm.target}
              onChange={e => {
                setCustomForm({ ...customForm, target: e.target.value });
                setVerifyResult(null);
              }}
              placeholder={t('models.manualModelPlaceholder')}
              list="custom-model-options"
            />
            <datalist id="custom-model-options">
              {safeModels
                .filter(m => {
                  if (!customForm.accountId) return true;
                  const account = safeAccounts.find(a => a.id === Number(customForm.accountId));
                  return account ? m.owned_by === account.provider_id : true;
                })
                .map(m => (
                  <option key={m.id} value={m.id} />
                ))}
            </datalist>
          </div>

          {/* Verify */}
          <div className="space-y-2">
            <Button
              type="button"
              variant="outline"
              onClick={handleVerify}
              disabled={isVerifying || !customForm.target || !customForm.accountId}
              className="bg-warning/10 text-warning hover:bg-warning/20 border-0"
            >
              {isVerifying ? <Loader2 size={16} className="animate-spin" /> : <Zap size={16} />}
              {isVerifying ? t('models.verifying') : t('models.verify')}
            </Button>
            {verifyResult && (
              <div className={cn(
                "flex items-center gap-2 p-3 rounded-lg text-sm font-medium",
                verifyResult.success ? "bg-success/10 text-success" : "bg-destructive/10 text-destructive"
              )}>
                {verifyResult.success ? <CheckCircle2 size={16} /> : <XCircle size={16} />}
                {verifyResult.success
                  ? `${t('models.verifySuccess')}${verifyResult.latency ? ` (${(verifyResult.latency / 1000).toFixed(1)}s)` : ''}`
                  : verifyResult.error || t('models.verifyFirst')}
              </div>
            )}
          </div>

          <div className="pt-2 flex gap-3">
            <Button type="button" variant="outline" onClick={() => { setIsCustomModalOpen(false); setVerifyResult(null); }} className="flex-1">{t('common.cancel')}</Button>
            <Button
              type="submit"
              disabled={!verifyResult?.success}
              className="flex-1"
            >
              <Save size={16} /> {t('common.save')}
            </Button>
          </div>
        </form>
      </Dialog>

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
    </div>
  );
}

