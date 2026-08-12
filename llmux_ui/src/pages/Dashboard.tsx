// 仪表盘（参考老项目）：固定视口高度 → 两栏等高（别名健康 + 最近动态），列表内部滚动
import React, { useState, useMemo } from "react";
import { healthApi } from "@/api/health";
import { activityApi } from "@/api/activity";
import { accountApi } from "@/api/accounts";
import { keyApi } from "@/api/keys";
import { modelApi } from "@/api/models";
import type { HealthEntry } from "@/types/health";
import type { ActivityEntry, ActivityResponse } from "@/types/activity";
import type { AccountPublic } from "@/types/account";
import type { ApiKey } from "@/types/key";
import type { AliasResponse, ModelHealthEntry } from "@/types/model";
import { Badge } from "@/components/ui/Badge";
import { StatusDot } from "@/components/shared/StatusDot";
import { SearchInput } from "@/components/ui/SearchInput";
import { Select } from "@/components/ui/Select";
import { PageHeader } from "@/components/shared/PageHeader";
import { Button } from "@/components/ui/Button";
import { formatTimestamp, formatLatency } from "@/utils/format";
import { useT } from "@/i18n";
import { useCachedData } from "@/hooks/useCachedData";
import { cn } from "@/utils/helpers";
import {
  Tags,
  KeyRound,
  Users,
  HeartPulse,
  LayoutDashboard,
  AlertTriangle,
  Activity,
  Database,
  RefreshCw,
  Plus,
} from "lucide-react";
import { useNavigate } from "react-router-dom";

// 仪表盘展示数据：6 路请求聚合，含单项错误收集
interface DashboardData {
  accounts: AccountPublic[];
  aliases: AliasResponse[];
  keys: ApiKey[];
  health: HealthEntry[];
  modelHealth: ModelHealthEntry[];
  activity: ActivityResponse | null;
  errors: string[];
}

export const Dashboard: React.FC = () => {
  const { t } = useT();
  const navigate = useNavigate();
  // 整包缓存：切回仪表盘直接展示旧数据，过期后后台刷新；快速请求不闪骨架
  const { data, loading, showSkeleton, refetch: loadAll } = useCachedData<DashboardData>(
    "dashboard",
    async () => {
      const errs: string[] = [];
      const [a, al, k, h, mh, act] = await Promise.allSettled([
        accountApi.list(),
        modelApi.getAliases(),
        keyApi.list(),
        healthApi.list(),
        modelApi.getHealth(),
        activityApi.list(100),
      ]);
      if (a.status === "rejected") errs.push(`Accounts: ${a.reason?.message}`);
      if (al.status === "rejected") errs.push(`Aliases: ${al.reason?.message}`);
      if (k.status === "rejected") errs.push(`Keys: ${k.reason?.message}`);
      if (h.status === "rejected") errs.push(`Health: ${h.reason?.message}`);
      if (mh.status === "rejected") errs.push(`ModelHealth: ${mh.reason?.message}`);
      if (act.status === "rejected") errs.push(`Activity: ${act.reason?.message}`);
      return {
        accounts: a.status === "fulfilled" ? a.value : [],
        aliases: al.status === "fulfilled" ? al.value : [],
        keys: k.status === "fulfilled" ? k.value : [],
        health: h.status === "fulfilled" ? h.value : [],
        modelHealth: mh.status === "fulfilled" ? mh.value : [],
        activity: act.status === "fulfilled" ? act.value : null,
        errors: errs,
      };
    },
    { ttlMs: 30_000 },
  );
  // 解构默认值用 useMemo 稳定引用，避免每次渲染生成新数组导致下游 useMemo 失效
  const accounts = useMemo(() => data?.accounts ?? [], [data]);
  const aliases = useMemo(() => data?.aliases ?? [], [data]);
  const keys = useMemo(() => data?.keys ?? [], [data]);
  const health = useMemo(() => data?.health ?? [], [data]);
  const modelHealth = useMemo(() => data?.modelHealth ?? [], [data]);
  const activity = useMemo(() => data?.activity ?? null, [data]);
  const errors = useMemo(() => data?.errors ?? [], [data]);
  const [aliasSearch, setAliasSearch] = useState("");
  const [aliasVendor, setAliasVendor] = useState("all");
  const [onlyShowErrors, setOnlyShowErrors] = useState(false);

  const healthyCount = health.filter((h) => h.status !== "down" && h.status !== "unknown").length;

  // 别名健康：modelHealth 按 model 取最优（success 优先 + 延迟低）
  const aliasHealthList = useMemo(() => {
    const bestByModel = new Map<string, ModelHealthEntry>();
    modelHealth.forEach((h) => {
      const ex = bestByModel.get(h.model);
      if (!ex || (h.success && h.latency < ex.latency) || (!ex.success && h.success)) bestByModel.set(h.model, h);
    });
    return aliases.map((a) => ({
      id: a.id,
      alias: a.alias,
      target_model: a.target_model,
      provider: a.vendor_id || "",
      success: bestByModel.get(a.target_model)?.success === 1,
      latency: bestByModel.get(a.target_model)?.latency ?? null,
    }));
  }, [aliases, modelHealth]);

  const filteredAliases = useMemo(() => {
    const q = aliasSearch.toLowerCase();
    const base = !aliasSearch
      ? aliasHealthList
      : aliasHealthList.filter(
          (a) => a.alias.toLowerCase().includes(q) || a.target_model.toLowerCase().includes(q) || a.provider.toLowerCase().includes(q),
        );
    return aliasVendor === "all" ? base : base.filter((a) => a.provider === aliasVendor);
  }, [aliasHealthList, aliasSearch, aliasVendor]);

  const providerList = useMemo(() => {
    return [...new Set(aliasHealthList.map((a) => a.provider).filter(Boolean))];
  }, [aliasHealthList]);

  // 最近动态（useMemo 保证引用稳定，避免下游 useMemo deps 每次变化）
  const entries = useMemo(() => activity?.entries ?? [], [activity]);
  const filteredLogs = useMemo(() => {
    const src = onlyShowErrors ? entries.filter((l) => l.success !== 1) : entries;
    return src.slice(0, 100);
  }, [entries, onlyShowErrors]);

  const logMetrics = useMemo(() => {
    const recent = entries.slice(0, 100);
    const ok = recent.filter((l) => l.success === 1).length;
    const rate = recent.length ? Math.round((ok / recent.length) * 100) : 0;
    const avg = recent.length ? Math.round(recent.reduce((s, l) => s + (l.latency_ms || 0), 0) / recent.length) : 0;
    return { rate, avg, len: recent.length };
  }, [entries]);

  return (
    <div
      className="flex flex-col gap-6"
      style={{ height: "calc(100dvh - 120px)", paddingBottom: "12px" }}
    >
      {/* Header */}
      <PageHeader
        icon={LayoutDashboard}
        iconClass="bg-primary/20 text-primary-foreground"
        title={t("dash.title")}
        description="LLMux · AI Gateway"
        actions={
          <>
            <Button variant="outline" size="sm" onClick={loadAll}>
              <RefreshCw className={cn("h-4 w-4", loading && "animate-spin text-primary")} />
              {t("common.refresh")}
            </Button>
            <Button size="sm" onClick={() => navigate("/accounts")}>
              <Plus className="h-4 w-4" /> {t("accounts.add")}
            </Button>
          </>
        }
      />

      {/* 后端不可用提示 */}
      {errors.length > 0 && (
        <div className="animate-fade-in rounded-2xl border border-destructive/30 bg-destructive/10 p-4">
          <p className="flex items-center gap-2 text-sm font-semibold text-destructive-foreground">
            <AlertTriangle className="h-4 w-4" /> {t("dash.error.title")}
          </p>
          <p className="mt-1 text-xs text-destructive-foreground/80">{t("dash.error.desc")}</p>
          <ul className="mt-2 space-y-1">
            {errors.map((err, i) => (
              <li key={i} className="font-mono text-xs text-destructive-foreground/70">
                {err}
              </li>
            ))}
          </ul>
        </div>
      )}

      {/* 4 统计卡 */}
      <div className="grid grid-cols-2 gap-4 lg:grid-cols-4">
        <StatCard label={t("dash.stats.accounts")} value={accounts.length} icon={Users} iconClass="bg-primary/20 text-primary-foreground" />
        <StatCard label={t("dash.stats.aliases")} value={aliases.length} icon={Tags} iconClass="bg-secondary/60 text-secondary-foreground" />
        <StatCard label={t("dash.stats.keys")} value={keys.length} icon={KeyRound} iconClass="bg-warning/25 text-warning-foreground" />
        <StatCard
          label={t("dash.stats.healthy")}
          value={healthyCount}
          sub={t("dash.stats.healthy.sub", { ok: healthyCount, total: accounts.length })}
          icon={HeartPulse}
          iconClass="bg-success/20 text-success-foreground"
        />
      </div>

      {/* 两栏等高面板 */}
      <div className="grid min-h-0 flex-1 grid-cols-1 gap-6 lg:grid-cols-12">
        {/* 左：别名健康（7 列） */}
        <section className="flex min-h-0 flex-col overflow-hidden rounded-2xl border border-border bg-card shadow-card lg:col-span-7">
          <div className="border-b border-border px-6 py-4">
            <div className="flex flex-wrap items-center justify-between gap-2">
              <div className="flex items-center gap-2">
                <Database className="h-4 w-4 text-muted-foreground" />
                <h2 className="text-base font-semibold text-card-foreground">{t("dash.aliases.title")}</h2>
              </div>
              <Badge variant="success" className="bg-success/10 text-success-foreground">
                {aliasHealthList.filter((a) => a.success).length}/{aliasHealthList.length} OK
              </Badge>
            </div>
            <div className="mt-3 flex flex-wrap gap-2">
              <SearchInput
                value={aliasSearch}
                onChange={setAliasSearch}
                placeholder={t("dash.aliases.searchPlaceholder")}
                className="w-full sm:w-56"
              />
              <div className="w-40">
                <Select
                  value={aliasVendor}
                  onChange={setAliasVendor}
                  options={[
                    { value: "all", label: t("dash.aliases.allVendors") },
                    ...providerList.map((v) => ({ value: v, label: v })),
                  ]}
                />
              </div>
            </div>
          </div>

          <div className="flex-1 divide-y divide-border/50 overflow-y-auto">
            {showSkeleton ? (
              <div className="space-y-2 p-4">
                {Array.from({ length: 5 }).map((_, i) => (
                  <div key={i} className="h-12 animate-pulse rounded-xl bg-muted" />
                ))}
              </div>
            ) : filteredAliases.length === 0 ? (
              <p className="py-10 text-center text-sm text-muted-foreground">{t("dash.aliases.empty")}</p>
            ) : (
              filteredAliases.map((a) => (
                <div
                  key={a.id}
                  className="flex items-center justify-between gap-4 px-5 py-3.5 transition-colors hover:bg-muted/30"
                >
                  <div className="flex min-w-0 items-center gap-3">
                    <StatusDot status={a.success ? "healthy" : a.latency != null ? "down" : "unknown"} />
                    <div className="min-w-0">
                      <div className="flex flex-wrap items-center gap-2">
                        <span className="truncate text-sm font-semibold text-card-foreground">{a.alias}</span>
                        {a.provider && (
                          <span className="rounded border border-border bg-muted px-2 py-0.5 text-xs font-medium text-muted-foreground">
                            {a.provider}
                          </span>
                        )}
                      </div>
                      <p className="mt-0.5 truncate font-mono text-xs text-muted-foreground">{a.target_model}</p>
                    </div>
                  </div>
                  <div className="shrink-0 text-right">
                    {a.latency != null ? (
                      <Badge
                        variant={a.success ? "success" : "danger"}
                        className={cn(
                          "font-mono",
                          a.success
                            ? a.latency < 500
                              ? "bg-success/10 text-success-foreground"
                              : a.latency < 1200
                                ? "bg-warning/10 text-warning-foreground"
                                : "bg-primary/10 text-primary-foreground"
                            : "bg-destructive/10 text-destructive-foreground",
                        )}
                      >
                        {a.success ? formatLatency(a.latency) : "ERR"}
                      </Badge>
                    ) : (
                      <span className="text-xs text-muted-foreground/60">—</span>
                    )}
                  </div>
                </div>
              ))
            )}
          </div>
        </section>

        {/* 右：最近动态（5 列） */}
        <section className="flex min-h-0 flex-col overflow-hidden rounded-2xl border border-border bg-card shadow-card lg:col-span-5">
          <div className="border-b border-border px-6 py-4">
            <div className="flex flex-wrap items-center justify-between gap-2">
              <div className="flex items-center gap-2">
                <Activity className="h-4 w-4 text-muted-foreground" />
                <h2 className="text-base font-semibold text-card-foreground">{t("dash.logs.title")}</h2>
              </div>
              <Button
                size="sm"
                variant={onlyShowErrors ? "danger" : "outline"}
                onClick={() => setOnlyShowErrors(!onlyShowErrors)}
              >
                <AlertTriangle className="h-3.5 w-3.5" />
                {onlyShowErrors ? t("dash.logs.showAll") : t("dash.logs.errorsOnly")}
              </Button>
            </div>

            <div className="mt-4 grid grid-cols-2 gap-4 rounded-xl border border-border/50 bg-muted p-4">
              <div>
                <span className="block text-xs font-medium uppercase tracking-wider text-muted-foreground">
                  {t("dash.logs.successRate")}
                </span>
                <span className="mt-1 block text-xl font-bold text-card-foreground">{logMetrics.rate}%</span>
              </div>
              <div>
                <span className="block text-xs font-medium uppercase tracking-wider text-muted-foreground">
                  {t("dash.logs.avgLatency")}
                </span>
                <span className="mt-1 block text-xl font-bold text-card-foreground">{avgText(logMetrics.avg)}</span>
              </div>
            </div>

            {/* 延迟脉搏 */}
            <div className="mt-4">
              <div className="mb-1.5 flex items-center justify-between">
                <span className="text-[10px] font-bold uppercase tracking-widest text-muted-foreground">
                  {t("dash.logs.latencyPulse")}
                </span>
                <span className="font-mono text-[10px] text-muted-foreground/60">{logMetrics.len}</span>
              </div>
              <div className="h-16 rounded-lg border border-border/40 bg-muted/40 p-2">
                {entries.length > 0 ? (
                  <PulseChart entries={entries} />
                ) : (
                  <div className="flex h-full items-center justify-center text-[10px] italic text-muted-foreground/40">
                    {t("dash.logs.empty")}
                  </div>
                )}
              </div>
            </div>
          </div>

          {/* 日志流（卡片式） */}
          <div className="flex-1 space-y-2 overflow-y-auto p-4">
            {showSkeleton ? (
              <div className="space-y-2">
                {Array.from({ length: 4 }).map((_, i) => (
                  <div key={i} className="h-14 animate-pulse rounded-xl bg-muted" />
                ))}
              </div>
            ) : filteredLogs.length === 0 ? (
              <p className="py-8 text-center text-sm text-muted-foreground">
                {onlyShowErrors ? t("dash.logs.noErrors") : t("dash.logs.empty")}
              </p>
            ) : (
              filteredLogs.map((log) => (
                <div
                  key={log.id}
                  className={cn(
                    "rounded-xl border p-3 text-xs transition-colors",
                    log.success === 1 ? "border-border/50 bg-muted/40 hover:bg-muted/70" : "border-destructive/10 bg-destructive/5 hover:bg-destructive/10",
                  )}
                >
                  <div className="flex flex-wrap items-start justify-between gap-2">
                    <div className="flex min-w-0 items-center gap-2">
                      <span className="shrink-0 text-muted-foreground/60">{timeText(log.timestamp)}</span>
                      <span className="truncate font-semibold text-card-foreground">{log.model}</span>
                    </div>
                    <div className="flex shrink-0 items-center gap-2">
                      <span className="font-mono text-muted-foreground">{log.latency_ms ? formatLatency(log.latency_ms) : "--"}</span>
                      <Badge
                        variant={log.success === 1 ? "success" : "danger"}
                        className={log.success === 1 ? "bg-success/15 text-success-foreground" : "bg-destructive/15 text-destructive-foreground"}
                      >
                        {log.success === 1 ? "200" : "ERR"}
                      </Badge>
                    </div>
                  </div>
                  {log.account_name && <div className="mt-1 text-muted-foreground/60">{log.account_name}</div>}
                  {log.success !== 1 && log.error_message && (
                    <div className="mt-2 rounded-lg border border-destructive/20 bg-destructive/10 p-2 leading-relaxed text-destructive-foreground">
                      {log.error_message}
                    </div>
                  )}
                </div>
              ))
            )}
          </div>
        </section>
      </div>
    </div>
  );
};

/** 马卡龙渐变统计卡：右上角粉彩图标 */
const StatCard: React.FC<{
  label: string;
  value: string | number;
  sub?: string;
  icon: React.ComponentType<{ className?: string }>;
  iconClass?: string;
}> = ({ label, value, sub, icon: Icon, iconClass = "bg-card/70 text-primary" }) => (
  <div className="macaron-gradient relative overflow-hidden rounded-2xl border border-border p-5 shadow-card">
    <div className={cn("absolute right-4 top-4 flex h-10 w-10 items-center justify-center rounded-xl shadow-soft", iconClass)}>
      <Icon className="h-5 w-5" />
    </div>
    <p className="text-sm font-medium text-muted-foreground">{label}</p>
    <p className="mt-1 text-3xl font-bold text-card-foreground">{value}</p>
    {sub && <p className="mt-0.5 text-xs text-muted-foreground">{sub}</p>}
  </div>
);

/** 延迟脉搏图：三档固定高度（正常绿=1 / >2s 橙=1.1 / 失败红=1.2，归一化后几乎等高），hover 显示日期+模型+延迟 */
const PulseChart: React.FC<{ entries: ActivityEntry[] }> = ({ entries }) => {
  const recent = entries.slice(0, 100).reverse();
  return (
    <div className="flex h-full items-end gap-[2px]">
      {recent.map((e) => {
        const h = !e.success ? "100%" : e.latency_ms > 2000 ? "92%" : "83%";
        const color = !e.success ? "bg-destructive/80" : e.latency_ms > 2000 ? "bg-warning/80" : "bg-success/80";
        return (
          <div
            key={e.id}
            className={cn("min-w-0 flex-1 rounded-t transition-colors", color)}
            style={{ height: h }}
            title={`${formatTimestamp(e.timestamp)} · ${e.model} · ${e.latency_ms}ms${e.success ? "" : " · failed"}`}
          />
        );
      })}
    </div>
  );
};

function avgText(ms: number): string {
  if (ms < 1000) return `${ms}ms`;
  return `${(ms / 1000).toFixed(1)}s`;
}

function timeText(ts: number): string {
  return new Date(ts * 1000).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit", hour12: false });
}
