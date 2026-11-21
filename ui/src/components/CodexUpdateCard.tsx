import React, { useCallback, useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { AlertTriangle, Bot, Loader2, RefreshCw, Trash2 } from 'lucide-react';
import { apiFetch } from '@/lib/api';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { CodexLoginDialog } from './CodexLoginDialog';

/// Codex 模型清单版本：状态 / 检查 / 应用 / 自动建别名 / 账户健康。
///
/// 只读写自己的偏好键（`codex_update_*` / `codex_client_version`），**不复用设置页
/// 的整份 localConfig** —— 那几个键在 settings 表里，一起 PUT 会把运行期字段写回。
/// 运行期状态（latest / last_good / 自动建过的别名）在 `codex_update_state` 单行表，
/// 只经 `/api/system/codex-update*` 读写。

type Prefs = {
  manual_version?: string | null;
  auto_check: boolean;
  auto_apply: boolean;
  auto_alias: boolean;
  interval_hours: number;
};

type AccountHealth = {
  id: number;
  alias: string;
  provider_id?: string;
  configured: boolean;
  token_expired: boolean;
  models_count: number;
  error?: string | null;
  updated_at: number;
};

type NewModel = {
  slug: string;
  account_ids: number[];
  account_aliases: string[];
  context_window?: number | null;
  suggested_alias?: string;
};

type AutoCreatedAlias = {
  alias: string;
  target_model?: string;
  created_at?: number;
};

type Status = {
  effective_version: string;
  effective_source: 'manual' | 'auto' | 'fallback' | string;
  latest_version?: string | null;
  last_good_version?: string | null;
  last_checked_at: number;
  last_error?: string | null;
  pending_reason?: string | null;
  discovered_source?: string | null;
  manual_version?: string | null;
  prefs: Prefs;
  accounts: AccountHealth[];
  new_models: NewModel[];
  stale_aliases: { alias: string; target_model: string }[];
  auto_created: AutoCreatedAlias[];
  cli?: { installed: boolean };
};

const sourceBadge = (source: string) =>
  source === 'manual'
    ? 'bg-primary/10 text-primary'
    : source === 'auto'
      ? 'bg-success/10 text-success'
      : 'bg-muted text-muted-foreground';

const fmtTime = (secs: number) =>
  secs > 0 ? new Date(secs * 1000).toLocaleString() : '—';

export const CodexUpdateCard: React.FC = () => {
  const { t } = useTranslation();
  const [status, setStatus] = useState<Status | null>(null);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [pinDraft, setPinDraft] = useState('');
  const [intervalDraft, setIntervalDraft] = useState('6');
  const [selected, setSelected] = useState<string[]>([]);
  const [loginAccount, setLoginAccount] = useState<{ id: number; alias: string } | null>(null);

  const load = useCallback(async () => {
    try {
      const res = await apiFetch('/api/system/codex-update');
      if (!res.ok) throw new Error(`${res.status} ${res.statusText}`);
      const data: Status = await res.json();
      setStatus(data);
      setPinDraft(data.prefs?.manual_version ?? '');
      setIntervalDraft(String(data.prefs?.interval_hours ?? 6));
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => { void load(); }, [load]);

  const run = async (key: string, path: string, body?: unknown) => {
    setBusy(key);
    setError(null);
    setNotice(null);
    try {
      const res = await apiFetch(path, {
        method: 'POST',
        headers: body === undefined ? undefined : { 'content-type': 'application/json' },
        body: body === undefined ? undefined : JSON.stringify(body),
      });
      const data = await res.json().catch(() => ({}));
      if (!res.ok) throw new Error(data?.error || `${res.status} ${res.statusText}`);
      return data;
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      return null;
    } finally {
      setBusy(null);
    }
  };

  // 只 PUT 自己的键：SettingsService::batch_set 是 upsert，不会动其它配置。
  const savePrefs = async (patch: Record<string, unknown>) => {
    setBusy('prefs');
    setError(null);
    try {
      const res = await apiFetch('/api/settings', {
        method: 'PUT',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify(patch),
      });
      if (!res.ok) throw new Error(`${res.status} ${res.statusText}`);
      await load();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(null);
    }
  };

  const checkNow = async () => {
    const data = await run('check', '/api/system/codex-update/check');
    if (!data) return;
    // auto_apply 打开时后台 check 会直接走应用，响应是 apply 的形状
    // （没有 latest_version/newer）；安全门拦下时同理。
    if (data.applied) {
      setNotice(t('codexUpdate.applied', { version: data.version ?? '' }));
      setSelected([]);
    } else if (data.pending) {
      setError(data.reason || t('codexUpdate.applyBlocked'));
    } else {
      const latest = data.latest_version ?? '—';
      setNotice(
        data.newer
          ? t('codexUpdate.checkFound', { version: latest })
          : t('codexUpdate.checkUpToDate', { version: latest }),
      );
    }
    await load();
  };

  const applyVersion = async (force: boolean) => {
    const version = status?.latest_version || undefined;
    const data = await run('apply', '/api/system/codex-update/apply', { version, force });
    if (!data) return;
    if (data.applied) {
      setNotice(t('codexUpdate.applied', { version: data.version ?? version ?? '' }));
      setSelected([]);
    } else if (data.pending) {
      setError(data.reason || t('codexUpdate.applyBlocked'));
    } else {
      setNotice(data.reason || t('codexUpdate.applyNoop'));
    }
    await load();
  };

  const createAliases = async () => {
    if (selected.length === 0) return;
    const data = await run('aliases', '/api/system/codex-update/aliases', { slugs: selected });
    if (data) {
      setNotice(t('codexUpdate.aliasesCreated', { count: data.created?.length ?? 0 }));
      setSelected([]);
      await load();
    }
  };

  const resetPin = async () => {
    const data = await run('reset', '/api/system/codex-update/reset');
    if (data) {
      setNotice(t('codexUpdate.pinCleared'));
      await load();
    }
  };

  // 撤销自动建的别名：先按别名查 id，再走既有 DELETE。只允许删「target 仍等于
  // 记录里的 target」的那条，避免误删用户后来改过的别名。
  const removeAutoAlias = async (row: AutoCreatedAlias) => {
    if (!window.confirm(t('codexUpdate.revokeConfirm', { alias: row.alias }))) return;
    setBusy(`revoke:${row.alias}`);
    setError(null);
    try {
      const res = await apiFetch('/api/models/aliases');
      if (!res.ok) throw new Error(`${res.status} ${res.statusText}`);
      const aliases: { id: number; alias: string; target_model: string }[] = await res.json();
      const hit = aliases.find(
        (a) => a.alias === row.alias && (!row.target_model || a.target_model === row.target_model),
      );
      if (!hit) {
        setNotice(t('codexUpdate.revokeGone', { alias: row.alias }));
        return;
      }
      const del = await apiFetch(`/api/models/aliases/${hit.id}`, { method: 'DELETE' });
      if (!del.ok) throw new Error(`${del.status} ${del.statusText}`);
      setNotice(t('codexUpdate.revoked', { alias: row.alias }));
      await load();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(null);
    }
  };

  const expiredAccounts = useMemo(
    () => (status?.accounts ?? []).filter((a) => a.token_expired || !a.configured),
    [status],
  );

  if (loading) {
    return (
      <div className="p-4 text-xs text-muted-foreground flex items-center gap-2">
        <Loader2 size={12} className="animate-spin" /> {t('common.loading')}
      </div>
    );
  }

  return (
    <div className="p-4 space-y-4">
      {/* 状态行 */}
      <div className="flex flex-col sm:flex-row sm:items-center justify-between gap-3">
        <div className="flex items-center gap-2 flex-wrap">
          <Bot size={16} className="text-primary" />
          <span className="text-sm font-semibold">{t('codexUpdate.effectiveVersion')}</span>
          <code className="text-xs font-mono px-1.5 py-0.5 rounded bg-muted">
            {status?.effective_version ?? '—'}
          </code>
          <span className={`text-[10px] font-bold px-1.5 py-0.5 rounded ${sourceBadge(status?.effective_source ?? 'fallback')}`}>
            {t(`codexUpdate.source.${status?.effective_source ?? 'fallback'}`)}
          </span>
          {status?.latest_version && status.latest_version !== status.effective_version && (
            <span className="text-[10px] font-bold px-1.5 py-0.5 rounded bg-warning/10 text-warning">
              {t('codexUpdate.latestAvailable', { version: status.latest_version })}
            </span>
          )}
        </div>
        <div className="flex items-center gap-2">
          <Button size="sm" variant="outline" disabled={busy !== null} onClick={checkNow}>
            {busy === 'check' ? <Loader2 size={12} className="animate-spin mr-1" /> : <RefreshCw size={12} className="mr-1" />}
            {t('codexUpdate.checkNow')}
          </Button>
          <Button size="sm" disabled={busy !== null} onClick={() => applyVersion(false)}>
            {busy === 'apply' ? <Loader2 size={12} className="animate-spin mr-1" /> : null}
            {t('codexUpdate.applyNow')}
          </Button>
        </div>
      </div>

      {/* 元信息 */}
      <div className="grid grid-cols-2 sm:grid-cols-4 gap-2 text-xs">
        {[
          [t('codexUpdate.latest'), status?.latest_version ?? '—'],
          [t('codexUpdate.lastGood'), status?.last_good_version ?? '—'],
          [t('codexUpdate.lastChecked'), fmtTime(status?.last_checked_at ?? 0)],
          [t('codexUpdate.cli'), status?.cli?.installed ? t('codexUpdate.cliInstalled') : t('codexUpdate.cliMissing')],
        ].map(([label, value]) => (
          <div key={label} className="p-2 rounded-lg border border-border/60 bg-muted/20">
            <div className="text-[10px] font-semibold text-muted-foreground uppercase tracking-wider">{label}</div>
            <div className="font-mono text-xs truncate" title={String(value)}>{value}</div>
          </div>
        ))}
      </div>

      {error && (
        <div className="flex items-start gap-2 text-xs text-destructive bg-destructive/5 border border-destructive/20 rounded-lg p-2">
          <AlertTriangle size={14} className="shrink-0 mt-0.5" />
          <div className="space-y-1">
            <div>{error}</div>
            {status?.pending_reason && (
              <Button size="sm" variant="outline" disabled={busy !== null} onClick={() => applyVersion(true)}>
                {t('codexUpdate.forceApply')}
              </Button>
            )}
          </div>
        </div>
      )}
      {!error && status?.last_error && (
        <div className="text-xs text-warning bg-warning/5 border border-warning/20 rounded-lg p-2">
          {t('codexUpdate.lastError')}: {status.last_error}
        </div>
      )}
      {!error && status?.pending_reason && (
        <div className="text-xs text-warning bg-warning/5 border border-warning/20 rounded-lg p-2">
          {t('codexUpdate.pending')}: {status.pending_reason}
        </div>
      )}
      {notice && (
        <div className="text-xs text-success bg-success/5 border border-success/20 rounded-lg p-2">{notice}</div>
      )}

      {/* 偏好 */}
      <div className="grid sm:grid-cols-2 gap-3 text-xs">
        <div className="p-3 rounded-lg border border-border/60 space-y-2">
          <div className="font-semibold">{t('codexUpdate.prefs')}</div>
          {([
            ['auto_check', 'codex_update_auto_check', 'autoCheck'],
            ['auto_apply', 'codex_update_auto_apply', 'autoApply'],
            ['auto_alias', 'codex_update_auto_alias', 'autoAlias'],
          ] as const).map(([field, key, label]) => (
            <div key={key} className="flex items-center justify-between gap-2">
              <span className="text-muted-foreground">{t(`codexUpdate.${label}`)}</span>
              <Button
                size="sm"
                variant={status?.prefs?.[field] ? 'default' : 'outline'}
                disabled={busy !== null}
                onClick={() => savePrefs({ [key]: !status?.prefs?.[field] })}
              >
                {status?.prefs?.[field] ? t('common.on', '开') : t('common.off', '关')}
              </Button>
            </div>
          ))}
          <div className="flex items-center justify-between gap-2">
            <span className="text-muted-foreground">{t('codexUpdate.interval')}</span>
            <div className="flex items-center gap-1">
              <Input
                type="number"
                min={1}
                max={168}
                value={intervalDraft}
                onChange={(e) => setIntervalDraft(e.target.value)}
                className="h-7 w-16 text-xs"
              />
              <Button
                size="sm"
                variant="outline"
                disabled={busy !== null}
                onClick={() => savePrefs({ codex_update_interval_hours: Number(intervalDraft) || 6 })}
              >
                {t('common.save')}
              </Button>
            </div>
          </div>
        </div>

        <div className="p-3 rounded-lg border border-border/60 space-y-2">
          <div className="font-semibold">{t('codexUpdate.manualPin')}</div>
          <p className="text-[11px] text-muted-foreground leading-relaxed">{t('codexUpdate.manualPinDesc')}</p>
          <div className="flex items-center gap-1">
            <Input
              value={pinDraft}
              onChange={(e) => setPinDraft(e.target.value)}
              placeholder="0.171.0"
              className="h-7 text-xs font-mono"
            />
            <Button
              size="sm"
              disabled={busy !== null}
              onClick={() => savePrefs({ codex_client_version: pinDraft.trim() })}
            >
              {t('common.save')}
            </Button>
            <Button size="sm" variant="outline" disabled={busy !== null} onClick={resetPin}>
              {t('codexUpdate.resetPin')}
            </Button>
          </div>
        </div>
      </div>

      {/* 账户健康 */}
      <div className="space-y-2">
        <div className="text-xs font-semibold text-muted-foreground uppercase tracking-widest">
          {t('codexUpdate.accounts')}
        </div>
        {status?.accounts?.length ? (
          <div className="border border-border/60 rounded-lg divide-y divide-border/40">
            {status.accounts.map((a) => (
              <div key={a.id} className="flex flex-col sm:flex-row sm:items-center justify-between gap-2 p-2 text-xs">
                <div className="flex items-center gap-2 flex-wrap">
                  <span className="font-semibold">{a.alias}</span>
                  <span className={`text-[10px] font-bold px-1.5 py-0.5 rounded ${a.configured ? 'bg-success/10 text-success' : 'bg-destructive/10 text-destructive'}`}>
                    {a.configured ? t('codexUpdate.configured') : t('codexUpdate.notConfigured')}
                  </span>
                  {a.token_expired && (
                    <span className="text-[10px] font-bold px-1.5 py-0.5 rounded bg-warning/10 text-warning">
                      {t('codexUpdate.tokenExpired')}
                    </span>
                  )}
                  <span className="text-[10px] text-muted-foreground">
                    {t('codexUpdate.modelCount', { count: a.models_count })} · {fmtTime(a.updated_at)}
                  </span>
                  {a.error && <span className="text-[10px] text-destructive truncate max-w-[240px]" title={a.error}>{a.error}</span>}
                </div>
                <Button
                  size="sm"
                  variant="outline"
                  onClick={() => setLoginAccount({ id: a.id, alias: a.alias })}
                >
                  {t('codexUpdate.relogin')}
                </Button>
              </div>
            ))}
          </div>
        ) : (
          <div className="text-xs text-muted-foreground">{t('codexUpdate.noAccounts')}</div>
        )}
        {expiredAccounts.length > 0 && (
          <div className="text-[11px] text-warning">{t('codexUpdate.expiredHint', { count: expiredAccounts.length })}</div>
        )}
      </div>

      {/* 新增模型 */}
      <div className="space-y-2">
        <div className="flex items-center justify-between">
          <div className="text-xs font-semibold text-muted-foreground uppercase tracking-widest">
            {t('codexUpdate.newModels')}
          </div>
          <Button size="sm" disabled={busy !== null || selected.length === 0} onClick={createAliases}>
            {busy === 'aliases' ? <Loader2 size={12} className="animate-spin mr-1" /> : null}
            {t('codexUpdate.createAliases', { count: selected.length })}
          </Button>
        </div>
        {status?.new_models?.length ? (
          <div className="border border-border/60 rounded-lg divide-y divide-border/40">
            {status.new_models.map((m) => (
              <label key={m.slug} className="flex items-center gap-3 p-2 text-xs cursor-pointer hover:bg-muted/20">
                <input
                  type="checkbox"
                  checked={selected.includes(m.slug)}
                  onChange={(e) =>
                    setSelected((prev) =>
                      e.target.checked ? [...prev, m.slug] : prev.filter((s) => s !== m.slug),
                    )
                  }
                />
                <code className="font-mono text-xs">{m.slug}</code>
                <span className="text-[10px] text-muted-foreground">{m.account_aliases.join(', ')}</span>
                {typeof m.context_window === 'number' && m.context_window > 0 && (
                  <span className="text-[10px] text-muted-foreground">
                    {t('codexUpdate.contextWindow', { count: m.context_window })}
                  </span>
                )}
              </label>
            ))}
          </div>
        ) : (
          <div className="text-xs text-muted-foreground">{t('codexUpdate.noNewModels')}</div>
        )}
      </div>

      {/* 失效别名（只报告，不删除） */}
      {status?.stale_aliases?.length ? (
        <div className="space-y-2">
          <div className="text-xs font-semibold text-warning uppercase tracking-widest">
            {t('codexUpdate.staleAliases')}
          </div>
          <div className="border border-warning/20 rounded-lg divide-y divide-warning/10">
            {status.stale_aliases.map((s) => (
              <div key={s.alias} className="p-2 text-xs text-warning">
                {s.alias} → <code className="font-mono">{s.target_model}</code>
              </div>
            ))}
          </div>
        </div>
      ) : null}

      {/* 自动创建记录 */}
      {status?.auto_created?.length ? (
        <div className="space-y-2">
          <div className="text-xs font-semibold text-muted-foreground uppercase tracking-widest">
            {t('codexUpdate.autoCreated')}
          </div>
          <div className="border border-border/60 rounded-lg divide-y divide-border/40">
            {status.auto_created.map((row, idx) => (
              <div key={`${row.alias}:${idx}`} className="flex items-center justify-between gap-2 p-2 text-xs">
                <div className="flex items-center gap-2 flex-wrap">
                  <code className="font-mono">{row.alias}</code>
                  <span className="text-[10px] text-muted-foreground">{fmtTime(row.created_at ?? 0)}</span>
                </div>
                <Button
                  size="sm"
                  variant="outline"
                  disabled={busy !== null}
                  onClick={() => removeAutoAlias(row)}
                >
                  {busy === `revoke:${row.alias}` ? (
                    <Loader2 size={12} className="animate-spin mr-1" />
                  ) : (
                    <Trash2 size={12} className="mr-1" />
                  )}
                  {t('codexUpdate.revoke')}
                </Button>
              </div>
            ))}
          </div>
        </div>
      ) : null}

      <CodexLoginDialog
        isOpen={loginAccount !== null}
        onClose={() => setLoginAccount(null)}
        account={loginAccount}
        onSignedIn={() => { void load(); }}
      />
    </div>
  );
};
