// 模型页（参考老项目）：顶部别名卡片区 + 下方模型网格，含拨测全部/新增/自定义别名
import React, { useEffect, useState, useCallback } from "react";
import { NavLink } from "react-router-dom";
import { modelApi } from "@/api/models";
import { accountApi } from "@/api/accounts";
import type {
  AvailableModel,
  AvailableModelsResponse,
  AliasResponse,
  ModelHealthEntry,
  TestQueueStatus,
} from "@/types/model";
import type { AccountPublic } from "@/types/account";
import { Button } from "@/components/ui/Button";
import { Badge } from "@/components/ui/Badge";
import { SearchInput } from "@/components/ui/SearchInput";
import { Tabs } from "@/components/ui/Tabs";
import { ConfirmDialog } from "@/components/ui/ConfirmDialog";
import { PageHeader } from "@/components/shared/PageHeader";
import { EmptyState } from "@/components/shared/EmptyState";
import { CopyButton } from "@/components/shared/CopyButton";
import { StatusDot } from "@/components/shared/StatusDot";
import { useToast } from "@/hooks/useToast";
import { useT } from "@/i18n";
import { usePolling } from "@/hooks/usePolling";
import { useDelayedLoading } from "@/hooks/useDelayedLoading";
import { cn } from "@/utils/helpers";
import { formatTimestamp, formatLatency } from "@/utils/format";
import {
  RefreshCw,
  Boxes,
  Zap,
  Loader2,
  CheckCircle2,
  XCircle,
  Plus,
  Trash2,
  ArrowRight,
  Tags,
  Play,
  Link2,
  X,
  AlertTriangle,
} from "lucide-react";
import { AliasFormModal, type AliasPayload } from "./AliasFormModal";
import { CustomAliasModal } from "./CustomAliasModal";

interface TestState {
  loading?: boolean;
  success?: boolean;
  latency?: number;
  error?: string;
}

export const ModelBrowser: React.FC = () => {
  const { t } = useT();
  const [data, setData] = useState<AvailableModelsResponse | null>(null);
  const [aliases, setAliases] = useState<AliasResponse[]>([]);
  const [accounts, setAccounts] = useState<AccountPublic[]>([]);
  const [healthMap, setHealthMap] = useState<Map<string, ModelHealthEntry>>(new Map());
  const [testResults, setTestResults] = useState<Record<string, TestState>>({});
  const [loading, setLoading] = useState(true);
  // 模型网格骨架延迟显示：快速请求不闪
  const showSkeleton = useDelayedLoading(loading, 200);
  const [search, setSearch] = useState("");
  const [vendorFilter, setVendorFilter] = useState<string>("all");
  const [page, setPage] = useState(1);
  const [queueStatus, setQueueStatus] = useState<TestQueueStatus | null>(null);
  const [testAllConfirm, setTestAllConfirm] = useState(false);

  // 别名 CRUD 状态
  const [formOpen, setFormOpen] = useState(false);
  const [editing, setEditing] = useState<AliasResponse | null>(null);
  const [linkTarget, setLinkTarget] = useState("");
  const [customOpen, setCustomOpen] = useState(false);
  const [saving, setSaving] = useState(false);
  const [deleteTarget, setDeleteTarget] = useState<AliasResponse | null>(null);
  const [deleting, setDeleting] = useState(false);
  const toast = useToast();

  const fetchModels = useCallback(async (force = false) => {
    try {
      setData(await modelApi.getAvailable(force));
    } catch {
      toast.error(t("models.loadFailed"));
    }
  }, [t, toast]);

  const fetchHealth = useCallback(async () => {
    try {
      const rows = await modelApi.getHealth();
      const map = new Map<string, ModelHealthEntry>();
      rows.forEach((row) => {
        const existing = map.get(row.model);
        if (!existing || (!existing.success && row.success)) map.set(row.model, row);
      });
      setHealthMap(map);
      // 同步到卡片测试状态（不覆盖正在拨测的）
      setTestResults((prev) => {
        const next = { ...prev };
        map.forEach((row, model) => {
          if (!next[model]?.loading) {
            next[model] = { success: !!row.success, latency: row.latency, error: row.error ?? undefined };
          }
        });
        return next;
      });
    } catch {
      /* 健康数据可选，忽略失败 */
    }
  }, []);

  const fetchAliases = useCallback(async () => {
    try {
      setAliases(await modelApi.getAliases());
    } catch {
      toast.error(t("aliases.loadFailed"));
    }
    accountApi.list().then(setAccounts).catch(() => undefined);
  }, [t, toast]);

  const fetchAll = useCallback(async () => {
    setLoading(true);
    await Promise.allSettled([fetchModels(), fetchAliases(), fetchHealth()]);
    setLoading(false);
  }, [fetchModels, fetchAliases, fetchHealth]);

  useEffect(() => {
    fetchAll();
  }, [fetchAll]);

  // 队列运行中轮询进度 + 每次轮询都刷新健康（含结束那次：isRunning 已变 false 时
  // 最后一批结果同样需要带入，不能依赖 isRunning 判断）
  usePolling(
    async () => {
      try {
        const status = await modelApi.getTestQueueStatus();
        setQueueStatus(status);
        fetchHealth();
      } catch {
        // 轮询失败：立即停止（isRunning 置 false），避免每 2s 抛未捕获异常且永远停不下来
        setQueueStatus({ isRunning: false, total: 0, current: 0, progress: 0 });
      }
    },
    2000,
    queueStatus?.isRunning ?? false,
  );

  // 兜底：队列极快完成（testAll 返回时已结束）时轮询可能从未 tick，
  // 由 queueStatus 变为结束时再补一次最终健康刷新
  useEffect(() => {
    if (queueStatus && !queueStatus.isRunning) {
      fetchHealth();
    }
  }, [queueStatus, fetchHealth]);

  const handleTest = async (m: AvailableModel) => {
    setTestResults((prev) => ({ ...prev, [m.id]: { loading: true } }));
    try {
      const res = await modelApi.test({ model: m.id, vendorId: m.owned_by });
      setTestResults((prev) => ({
        ...prev,
        [m.id]: { loading: false, success: res.success, latency: res.latency, error: res.error ?? undefined },
      }));
    } catch (err) {
      setTestResults((prev) => ({
        ...prev,
        [m.id]: { loading: false, success: false, error: err instanceof Error ? err.message : undefined },
      }));
    }
  };

  // ── 拨测全部（测试已配置别名的模型） ─────────────────────
  const executeTestAll = async () => {
    const modelsToTest = aliases
      .map((a) => ({ model: a.target_model, vendorId: a.vendor_id || "" }))
      .filter((m) => m.model);
    if (modelsToTest.length === 0) {
      toast.error(t("models.testAllNoAliases"));
      setTestAllConfirm(false);
      return;
    }
    try {
      await modelApi.testAll({ models: modelsToTest });
      toast.success(t("models.testAllStarted"));
      setQueueStatus(await modelApi.getTestQueueStatus());
    } catch (err) {
      toast.error(err instanceof Error ? err.message : t("test.testFailed"));
    } finally {
      setTestAllConfirm(false);
    }
  };

  // ── 别名 CRUD ──────────────────────────────────────────
  const openCreate = () => {
    setEditing(null);
    setLinkTarget("");
    setFormOpen(true);
  };
  const openEdit = (alias: AliasResponse) => {
    setEditing(alias);
    setLinkTarget("");
    setFormOpen(true);
  };
  // 模型卡片快速关联：新增别名弹窗，目标模型预填
  const openLink = (model: AvailableModel) => {
    setEditing(null);
    setLinkTarget(model.id);
    setFormOpen(true);
  };

  const handleAliasSubmit = async (payload: AliasPayload) => {
    setSaving(true);
    try {
      // 编辑且改了别名名 → 先建新名成功后再删旧记录（避免 create 失败丢原别名）
      if (editing && payload.alias !== editing.alias) {
        await modelApi.createAlias(payload);
        await modelApi.deleteAlias(editing.id);
      } else {
        await modelApi.createAlias(payload);
      }
      toast.success(editing ? t("aliases.updated") : t("aliases.created"));
      setFormOpen(false);
      setEditing(null);
      fetchAliases();
    } catch (err) {
      toast.error(err instanceof Error ? err.message : t("aliases.createFailed"));
    } finally {
      setSaving(false);
    }
  };

  const handleCustomSubmit = async (payload: AliasPayload) => {
    setSaving(true);
    try {
      await modelApi.createAlias(payload);
      toast.success(t("aliases.created"));
      setCustomOpen(false);
      fetchAliases();
    } catch (err) {
      toast.error(err instanceof Error ? err.message : t("aliases.createFailed"));
    } finally {
      setSaving(false);
    }
  };

  const handleDelete = async () => {
    if (!deleteTarget) return;
    setDeleting(true);
    try {
      await modelApi.deleteAlias(deleteTarget.id);
      toast.success(t("aliases.deleted"));
      setDeleteTarget(null);
      fetchAliases();
    } catch (err) {
      toast.error(err instanceof Error ? err.message : t("aliases.deleteFailed"));
    } finally {
      setDeleting(false);
    }
  };

  // 已关联别名的模型集合（卡片边框标识）
  const linkedModels = new Set(aliases.map((a) => a.target_model));

  // 厂商过滤
  const vendors = data ? [...new Set(data.data.map((m) => m.owned_by).filter(Boolean))] : [];
  const filtered = (data?.data ?? []).filter((m) => {
    const matchSearch =
      !search ||
      (m.id ?? "").toLowerCase().includes(search.toLowerCase()) ||
      (m.name ?? "").toLowerCase().includes(search.toLowerCase());
    const matchVendor = vendorFilter === "all" || m.owned_by === vendorFilter;
    return matchSearch && matchVendor;
  });

  // 分页：每页限制渲染数量，避免模型过多 DOM 卡顿
  const PAGE_SIZE = 24;
  const totalPages = Math.max(1, Math.ceil(filtered.length / PAGE_SIZE));
  const pagedModels = filtered.slice((page - 1) * PAGE_SIZE, page * PAGE_SIZE);

  return (
    <div className="space-y-6">
      <PageHeader
        icon={Boxes}
        iconClass="bg-primary/20 text-primary-foreground"
        title={t("models.title")}
        description={
          data && (
            <>
              {t("models.count", { n: data.data.length })}
              {data.stale && (
                <Badge variant="warning" className="ml-2">
                  {t("models.cacheStale")}
                </Badge>
              )}
              <span className="ml-2 text-xs text-muted-foreground/70">
                {t("models.cachedAt", { ts: formatTimestamp(data.cached_at) })}
              </span>
            </>
          )
        }
        actions={
          <>
            <Button
              variant="outline"
              onClick={() => setTestAllConfirm(true)}
              disabled={queueStatus?.isRunning}
              className="border-warning/40 text-warning-foreground hover:bg-warning/10"
            >
              {queueStatus?.isRunning ? (
                <>
                  <Loader2 className="h-4 w-4 animate-spin" />
                  {t("models.testingQueue", { current: queueStatus.current, total: queueStatus.total })}
                </>
              ) : (
                <>
                  <Play className="h-4 w-4" />
                  {t("models.testAll")}
                </>
              )}
            </Button>
            <Button
              variant="ghost"
              className="text-muted-foreground"
              onClick={() => fetchModels(true)}
              loading={loading}
            >
              <RefreshCw className="h-4 w-4" />
            </Button>
            <Button variant="outline" onClick={() => setCustomOpen(true)}>
              <Zap className="h-4 w-4" /> {t("aliases.custom")}
            </Button>
            <Button onClick={openCreate}>
              <Plus className="h-4 w-4" /> {t("aliases.add")}
            </Button>
          </>
        }
      />

      {/* 模型子导航：模型库 / 拨测 / 健康 */}
      <div className="flex items-center gap-1.5">
        <NavLink
          to="/models"
          end
          className={({ isActive }) =>
            cn(
              "rounded-full px-3 py-1.5 text-xs font-medium transition-colors",
              isActive ? "bg-primary text-primary-foreground shadow-soft" : "text-muted-foreground hover:bg-muted hover:text-foreground",
            )
          }
        >
          {t("models.title")}
        </NavLink>
        <NavLink
          to="/models/health"
          className={({ isActive }) =>
            cn(
              "rounded-full px-3 py-1.5 text-xs font-medium transition-colors",
              isActive ? "bg-primary text-primary-foreground shadow-soft" : "text-muted-foreground hover:bg-muted hover:text-foreground",
            )
          }
        >
          {t("health.title")}
        </NavLink>
      </div>

      {/* ── 别名 Section ── */}
      {aliases.length > 0 && (
        <div className="space-y-3">
          <h2 className="flex items-center gap-2 px-1 text-xs font-semibold uppercase tracking-[0.2em] text-muted-foreground">
            <Tags className="h-3.5 w-3.5 text-primary" />
            {t("aliases.title")}
          </h2>
          <div className="grid grid-cols-1 gap-3 sm:grid-cols-2 md:grid-cols-3">
            {aliases.map((alias) => (
              <AliasCard
                key={alias.id}
                alias={alias}
                health={healthMap.get(alias.target_model)}
                onEdit={() => openEdit(alias)}
                onDelete={() => setDeleteTarget(alias)}
              />
            ))}
          </div>
        </div>
      )}

      {/* ── 模型库 ── */}
      <div className="space-y-3">
        <h2 className="flex items-center gap-2 px-1 text-xs font-semibold uppercase tracking-[0.2em] text-muted-foreground">
          <Boxes className="h-3.5 w-3.5 text-primary" />
          {t("models.title")}
        </h2>

        <div className="flex flex-wrap items-center gap-3">
          <SearchInput
            value={search}
            onChange={(v) => {
              setSearch(v);
              setPage(1);
            }}
            placeholder={t("models.searchPlaceholder")}
            className="w-full sm:w-64"
          />
          <div className="min-w-0 flex-1 overflow-x-auto">
            <Tabs
              active={vendorFilter}
              onChange={(v) => {
                setVendorFilter(v);
                setPage(1);
              }}
              items={[
                { key: "all", label: t("models.allVendors") },
                ...vendors.map((v) => ({ key: v, label: v })),
              ]}
            />
          </div>
        </div>
      </div>

      {/* Model Grid */}
      {showSkeleton ? (
        <div className="grid grid-cols-1 gap-3 md:grid-cols-2 lg:grid-cols-3">
          {Array.from({ length: 6 }).map((_, i) => (
            <div key={i} className="h-28 animate-pulse rounded-2xl bg-muted" />
          ))}
        </div>
      ) : filtered.length > 0 ? (
        <div className="grid grid-cols-1 items-start gap-3 md:grid-cols-2 lg:grid-cols-3">
          {pagedModels.map((model) => (
            <ModelCard
              key={`${model.owned_by}-${model.id}`}
              model={model}
              testState={testResults[model.id]}
              linked={linkedModels.has(model.id)}
              onTest={() => handleTest(model)}
              onLink={() => openLink(model)}
            />
          ))}
        </div>
      ) : (
        <EmptyState icon={Boxes} title={t("common.noData")} />
      )}

      {/* 分页 */}
      {filtered.length > PAGE_SIZE && (
        <div className="flex flex-wrap items-center justify-between gap-2">
          <span className="text-xs text-muted-foreground">{t("models.total", { n: filtered.length })}</span>
          <div className="flex items-center gap-2">
            <Button size="sm" variant="outline" disabled={page <= 1} onClick={() => setPage((p) => p - 1)}>
              {t("common.previous")}
            </Button>
            <span className="px-1 text-xs text-muted-foreground">{t("models.page", { page, total: totalPages })}</span>
            <Button size="sm" variant="outline" disabled={page >= totalPages} onClick={() => setPage((p) => p + 1)}>
              {t("common.next")}
            </Button>
          </div>
        </div>
      )}

      {/* 弹窗 */}
      <AliasFormModal
        open={formOpen}
        editing={editing}
        models={data?.data ?? []}
        accounts={accounts}
        saving={saving}
        initialTarget={linkTarget}
        onClose={() => {
          setFormOpen(false);
          setEditing(null);
          setLinkTarget("");
        }}
        onSubmit={handleAliasSubmit}
      />
      <CustomAliasModal
        open={customOpen}
        models={data?.data ?? []}
        accounts={accounts}
        saving={saving}
        onClose={() => setCustomOpen(false)}
        onSubmit={handleCustomSubmit}
      />
      <ConfirmDialog
        open={!!deleteTarget}
        title={t("aliases.delete.title")}
        message={t("aliases.delete.confirm", {
          alias: deleteTarget?.alias ?? "",
          target: deleteTarget?.target_model ?? "",
        })}
        danger
        loading={deleting}
        onConfirm={handleDelete}
        onCancel={() => setDeleteTarget(null)}
      />
      <ConfirmDialog
        open={testAllConfirm}
        title={t("models.testAllConfirmTitle")}
        message={t("models.testAllConfirmMsg")}
        onConfirm={executeTestAll}
        onCancel={() => setTestAllConfirm(false)}
      />
    </div>
  );
};

/** 别名卡片：别名徽标 → 目标模型 + 状态点/延迟 + 绑定账户 chips */
const AliasCard: React.FC<{
  alias: AliasResponse;
  health?: ModelHealthEntry;
  onEdit: () => void;
  onDelete: () => void;
}> = ({ alias, health, onEdit, onDelete }) => {
  const { t } = useT();
  return (
    <div
      onClick={onEdit}
      className="group cursor-pointer rounded-2xl border border-border bg-card p-3.5 shadow-soft transition-all duration-200 hover:border-primary/40 hover:shadow-card"
    >
      <div className="flex items-center justify-between gap-2">
        <div className="flex min-w-0 items-center gap-1.5">
          <span className="shrink-0 rounded bg-primary/10 px-2 py-0.5 text-xs font-bold uppercase text-primary">
            {alias.alias}
          </span>
          <CopyButton text={alias.alias} className="shrink-0 opacity-0 transition-opacity group-hover:opacity-100" />
        </div>
        <Button
          size="sm"
          variant="ghost"
          className="h-6 w-6 shrink-0 p-0 text-muted-foreground opacity-0 transition-all hover:text-destructive group-hover:opacity-100"
          onClick={(e) => {
            e.stopPropagation();
            onDelete();
          }}
        >
          <Trash2 className="h-3.5 w-3.5" />
        </Button>
      </div>

      <div className="mt-1.5 flex min-w-0 items-center gap-1.5">
        <ArrowRight className="h-3 w-3 shrink-0 text-muted-foreground/40" />
        <span className="truncate font-mono text-xs font-semibold text-muted-foreground">{alias.target_model}</span>
        {health && (
          <div className="flex shrink-0 items-center gap-1">
            <StatusDot status={health.success ? "healthy" : "down"} className="h-1.5 w-1.5" />
            {health.latency != null && (
              <span className="text-[10px] text-muted-foreground/60">{formatLatency(health.latency)}</span>
            )}
          </div>
        )}
      </div>

      <div className="mt-2 flex flex-wrap gap-1">
        {alias.accounts.length > 0 ? (
          alias.accounts.map((acc) => (
            <span
              key={acc.id}
              className={cn(
                "rounded-full border px-1.5 py-0.5 text-[10px]",
                acc.is_preferred
                  ? "border-primary/40 bg-primary/10 text-primary"
                  : "border-border bg-muted/40 text-muted-foreground",
              )}
            >
              [{acc.vendor_id}] {acc.name}
              {acc.is_preferred && ` · ${t("aliases.preferred")}`}
            </span>
          ))
        ) : (
          <span className="text-[10px] text-muted-foreground/60">{t("aliases.prefixFallback")}</span>
        )}
      </div>
    </div>
  );
};

/** 模型卡片：厂商 + 状态点 + 模型名(复制) + 延迟 + 拨测/关联；已关联模型淡紫细边框 */
const ModelCard: React.FC<{
  model: AvailableModel;
  testState?: TestState;
  linked?: boolean;
  onTest: () => void;
  onLink: () => void;
}> = ({ model, testState, linked, onTest, onLink }) => {
  const { t } = useT();
  const [showError, setShowError] = useState(false);
  const isPlaceholder = model.id.endsWith("-models-unavailable");
  return (
    <div
      className={cn(
        "rounded-2xl border p-4 shadow-soft transition-all duration-200 hover:-translate-y-1 hover:shadow-card",
        isPlaceholder
          ? "border-dashed border-warning/40 bg-warning/5"
          : linked
            ? "border-primary/35 bg-card"
            : "border-border bg-card",
      )}
    >
      <div className="flex items-center justify-between gap-2">
        <span className="flex items-center gap-1.5 text-xs font-bold uppercase tracking-widest text-primary">
          {linked && <Link2 className="h-3 w-3 text-primary/50" />}
          {model.owned_by}
        </span>
        <div className="flex items-center gap-1.5">
          {testState?.loading ? (
            <Loader2 className="h-3.5 w-3.5 animate-spin text-primary" />
          ) : testState?.success ? (
            <CheckCircle2 className="h-3.5 w-3.5 text-success" />
          ) : testState && !testState.success ? (
            <XCircle className="h-3.5 w-3.5 text-destructive" />
          ) : null}
          {/* 错误图标：不占主布局，点击弹出完整错误浮层 */}
          {(model.error || testState?.error) && (
            <div className="relative">
              <button
                type="button"
                onClick={() => setShowError((v) => !v)}
                className="text-destructive-foreground transition-opacity hover:opacity-70"
                aria-label={t("models.viewError")}
                title={t("models.viewError")}
              >
                <AlertTriangle className="h-3.5 w-3.5" />
              </button>
              {showError && (
                <div className="absolute right-0 top-full z-10 mt-1.5 min-w-[300px] max-w-md animate-fade-in rounded-xl border border-destructive/30 bg-card p-3 shadow-card">
                  <div className="flex items-start justify-between gap-2">
                    <p className="min-w-0 whitespace-pre-wrap break-words text-xs leading-relaxed text-foreground">
                      {testState?.error ?? model.error}
                    </p>
                    <button
                      type="button"
                      onClick={() => setShowError(false)}
                      className="shrink-0 rounded-lg p-0.5 text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
                      aria-label="Close error detail"
                    >
                      <X className="h-3.5 w-3.5" />
                    </button>
                  </div>
                </div>
              )}
            </div>
          )}
          <Badge variant={isPlaceholder ? "warning" : linked ? "info" : "neutral"}>{model.owned_by}</Badge>
        </div>
      </div>

      <div className="mt-2 flex items-start justify-between gap-2">
        <h3 className="line-clamp-2 text-sm font-semibold leading-snug text-card-foreground" title={model.name}>
          {model.name || model.id}
        </h3>
        <CopyButton text={model.id} className="mt-0.5 shrink-0" />
      </div>
      <p className="mt-0.5 truncate font-mono text-xs text-muted-foreground">{model.id}</p>

      <div className="mt-2">
        {testState?.loading ? (
          <span className="text-xs text-primary">{t("models.testing")}</span>
        ) : testState?.latency != null ? (
          <span className="font-mono text-xs text-muted-foreground">{formatLatency(testState.latency)}</span>
        ) : (
          <span className="text-xs text-muted-foreground/50">—</span>
        )}
      </div>

      <div className="mt-3 flex items-center gap-2">
        <Button size="sm" variant="outline" onClick={onTest} disabled={testState?.loading || isPlaceholder} className="flex-1">
          <Zap className="h-3.5 w-3.5" /> {t("models.test")}
        </Button>
        <Button size="sm" variant="ghost" onClick={onLink} disabled={isPlaceholder} className="flex-1">
          <Link2 className="h-3.5 w-3.5" /> {t("models.associate")}
        </Button>
      </div>
    </div>
  );
};
