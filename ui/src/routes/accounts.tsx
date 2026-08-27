import React, { useEffect, useMemo, useState } from 'react';
import { useAccountsStore } from '../stores/accounts';
import { useModelsStore } from '../stores/models';
import { apiFetch } from '@/lib/api';
import {
  Users,
  Trash2,
  Plus,
  Settings2,
  Key,
  Loader2,
  AlertCircle,
  Save,
  Monitor,
  Copy,
  CheckCircle2,
  Pencil,
  ShieldAlert,
  Power,
  Eye,
  EyeOff,
  ChevronDown,
  Search,
  Filter,
  RefreshCw,
  Info,
} from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { Dialog, ConfirmDialog } from '../components/Modal';
import { CopyButton } from '../components/CopyButton';
import { cn } from '../lib/utils';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { StatusDot } from '@/components/shared/StatusDot';
import { ToggleGroup, ToggleGroupItem } from '@/components/ui/toggle-group';

const PROTOCOLS = ['chat', 'responses', 'messages'] as const;
type Protocol = typeof PROTOCOLS[number];

// 需要网页登录凭据（Cookie/Token）而非 API Key 的余额查询类型。
const COOKIE_KINDS = ['copilot', 'commandcode', 'opencode', 'opencode-go', 'opencode-zen', 'deepseek'];

// 各类型的凭据获取教程（小叹号展开）——精确到 Cookie 名。
const BALANCE_AUTH_HELP: Record<string, string[]> = {
  copilot: [
    'GitHub Copilot 使用 GitHub OAuth token（gho_/ghp_ 开头）查询配额。',
    '1. 终端执行 gh auth login 登录 GitHub；',
    '2. 执行 gh auth token 复制输出；',
    '3. 或 GitHub → Settings → Developer settings → Personal access tokens（账号需已开通 Copilot）。',
  ],
  commandcode: [
    'CommandCode 使用浏览器登录后的 Cookie 查询额度（只认一条，精确到名）。',
    '1. 浏览器登录 https://commandcode.ai；',
    '2. 按 F12 → Application（应用）→ Storage → Cookies → https://commandcode.ai；',
    '3. 找到 Name = __Secure-commandcode_prod_.session_token 这一行，双击 Value 复制；',
    '4. 若 Value 含 %2B/%3D 请保持原样粘贴（服务端只认编码态，解码成 + 会 401）；',
    '5. 可粘 “__Secure-commandcode_prod_.session_token=VALUE” 整串，也可只粘 VALUE。',
    '补充：__Secure-commandcode_prod_.session_data 是 JWT 明文、__stripe_*/cid 只是埋点，全部无效。',
  ],
  opencode: [
    'OpenCode 使用网页端的会话 Cookie 查询用量（精确到名：auth）。',
    '1. 浏览器登录 https://opencode.ai；',
    '2. 按 F12 → Application → Storage → Cookies → https://opencode.ai；',
    '3. 找到 Name = auth 这一行，双击 Value 复制（以 Fe26.2** 开头）；',
    '4. 可粘 “auth=VALUE” 整串，也可只粘 VALUE，都会自动补齐为 auth=VALUE；',
    '5. 若提示“未找到 workspace”，说明 Cookie 已过期，请重新登录后复制最新的 auth。',
    '说明：订阅为空时会回落到 Zen/Pay-as-you-go 钱包（customerID + balance / 1e8）。',
  ],
  'opencode-go': [
    'OpenCode Go 有两种认证，二选一（在下方输入框填写）：',
    '· API 模式（推荐）：粘贴 Go 的 API Key（op_…/sk-…，从 opencode.ai 开发者设置获取），走 GET /zen/go/v1/usage，无需 Cookie；',
    '· Cookie 模式：与 OpenCode 共用，F12 → Application → Storage → Cookies → https://opencode.ai → 复制 Name=auth（Fe26.2** 开头）的 Value，可粘 auth=VALUE 或裸 VALUE；',
    '⚠️ 若在此填 Cookie 却去请求 Go API，会报 401 Missing API key — 请确认所选“OpenCode Go”的凭据类型与上方二选一一致。',
    '账号 zen 推荐选 OpenCode Go；若批量放在 Go 系聚合下，可对每个 Go 账号单独配此凭据。',
  ],
  'opencode-zen': [
    'OpenCode Zen 为独立的 Pay-as-you-go 钱包余额（customerID + balance / 1e8），不走订阅窗口。',
    '1. 浏览器登录 https://opencode.ai；',
    '2. 按 F12 → Application → Storage → Cookies → https://opencode.ai；',
    '3. 找到 Name = auth 这一行，双击 Value 复制（以 Fe26.2** 开头）；',
    '4. 可粘 “auth=VALUE” 整串，也可只粘 VALUE，都会自动补齐为 auth=VALUE；',
    '5. 选用“OpenCode Zen”后仅查询钱包余额（与 OpenCode/Go 分离，需单独配置）。',
  ],
  deepseek: [
    'DeepSeek 已切换为平台 Bearer 全链路（需 Authorization: Bearer QSh4...，不是 Cookie .thumbcache）。',
    '1. 浏览器登录 https://platform.deepseek.com/usage（已登录态）→ F12 → Network → 刷新；',
    '2. 找到任意 /api/v0/users/get_user_summary 或 /api/v0/usage/by_api_key/cost 的请求 → Request Headers → 复制 authorization: Bearer 后的那串（以 QSh4... 开头）；',
    '3. 粘到下方余额认证框（可带 Bearer 前缀，系统会自动剥离）；将用 Bearer 拉 get_user_summary（余额 ¥ + 累计 ¥）与 by_api_key/cost?start=&end=&tz=28800（本日/本月 ¥），失败回落到 api.deepseek.com/user/balance；',
    '4. 若 Network 里没有 Bearer，改去 Application → Local Storage → https://platform.deepseek.com 搜 token 复制。',
  ],
};

// 余额认证输入框：上方清晰说明 + 小叹号展开详细教程。
function BalanceAuthInput({ kind, value, onChange, disabled }: { kind: string; value: string; onChange: (v: string) => void; disabled?: boolean }) {
  const { t } = useTranslation();
  const [helpOpen, setHelpOpen] = useState(false);
  const help = BALANCE_AUTH_HELP[kind] ?? [];
  const isGo = kind === 'opencode-go';
  return (
    <div className="space-y-2 rounded-md border border-border/70 bg-muted/20 p-3 min-w-0">
      <div className="flex items-start gap-2">
        <p className="flex-1 min-w-0 text-xs text-muted-foreground leading-relaxed">
          {isGo
            ? t('accounts.balanceAuthExplainGo', 'OpenCode Go 支持 API Key 或 Cookie 二选一：填 API Key 走 Go API（推荐），填 auth Cookie 走网页用量；请与上方“余额查询接口”的选择保持一致。')
            : t('accounts.balanceAuthExplain', '该类型的余额接口需要网页登录凭据（Cookie / Token），仅用于余额查询，不影响正常请求转发。留空则使用上方 API Key 查询；编辑时留空表示不修改。')}
        </p>
        <button
          type="button"
          aria-label="Balance auth tutorial"
          onClick={() => setHelpOpen(o => !o)}
          className={`shrink-0 p-1 rounded-full border transition-colors ${helpOpen ? 'text-primary border-primary/40' : 'text-muted-foreground border-border hover:text-foreground'}`}
        >
          <Info size={14} />
        </button>
      </div>
      <input
        type="text"
        value={value}
        disabled={disabled}
        onChange={e => onChange(e.target.value)}
        placeholder={isGo ? t('accounts.balanceAuthPlaceholderGo', '粘贴 Go API Key（op_…/sk-…）或 auth Cookie（Fe26.2**…）…') : t('accounts.balanceAuthPlaceholder', '粘贴 Cookie 整串或 Token…')}
        className="w-full min-w-0 h-9 px-3 rounded-md border border-input bg-background text-sm font-mono"
      />
      {helpOpen && (
        <div className="space-y-1 text-xs text-muted-foreground leading-relaxed">
          {help.map((line, i) => (
            <p key={i} className={i === 0 ? 'font-medium text-foreground/90' : ''}>{line}</p>
          ))}
        </div>
      )}
    </div>
  );
}

function formatResetAt(resetsAt?: number): string {
  if (!resetsAt) return '';
  return formatAbsMs(resetsAt);
}

function formatAbsMs(ms: number): string {
  const t = ms > 1e12 ? ms : ms * 1000;
  if (Number.isNaN(new Date(t).getTime())) return '';
  // 统一按东八区（UTC+8）展示，与后端 format_abs_ms 一致
  const cstMs = t + 8 * 3600 * 1000;
  const d = new Date(cstMs);
  const pad = (n: number) => String(n).padStart(2, '0');
  return `${pad(d.getUTCMonth() + 1)}月${pad(d.getUTCDate())}日 ${pad(d.getUTCHours())}:${pad(d.getUTCMinutes())}`;
}

// Normalized balance payload from GET /api/accounts/:id/balance (llmux-core::balance).
interface BalanceResult {
  provider: string;
  ok: boolean;
  summary?: string;
  detail?: string;
  windows?: { label: string; percent: number; resets_at?: number; exceeded?: boolean }[];
  rows?: { label: string; value: string }[];
  error?: string;
}

// Mirror of llmux-core join_upstream_url: merge path segments, dropping an
// adjacent duplicate "v1" (config carries the version segment).
const ENDPOINT_SUFFIX: Record<Protocol, string> = {
  chat: 'chat/completions',
  responses: 'responses',
  messages: 'v1/messages',
};

function joinUpstreamUrl(base: string, endpoint: string): string {
  try {
    const url = new URL(base.trim());
    const segments: string[] = [];
    for (const seg of url.pathname.split('/').filter(Boolean)) {
      if (seg === 'v1' && segments[segments.length - 1] === 'v1') continue;
      segments.push(seg);
    }
    for (const seg of endpoint.split('/').filter(Boolean)) {
      if (seg === 'v1' && segments[segments.length - 1] === 'v1') continue;
      segments.push(seg);
    }
    url.pathname = '/' + segments.join('/');
    return url.toString().replace(/\/+$/, '');
  } catch {
    return '';
  }
}

function resolvedEndpointUrl(base: string, proto: Protocol): string {
  if (!base.trim()) return '';
  return joinUpstreamUrl(base, ENDPOINT_SUFFIX[proto]);
}

// 余额/用量行：折叠态常显百分比（无时间，已耗尽灰色），展开态每行独立时间（未耗尽过期灰/已耗尽重置红，东八区）
function BalanceLine({ account: acc, balance, loading, unsupported, onRefresh }: {
  account: { id: number; is_active: number };
  balance?: BalanceResult;
  loading: boolean;
  unsupported: boolean;
  onRefresh: () => void;
}) {
  const [open, setOpen] = useState(false);
  const hasData = !!balance;
  const showBtn = !unsupported;
  // D: 前端二次排序兜底 — 5小时 → 周 → 月（兼容滚动/每周等别名）
  const sortedWindows = useMemo(() => {
    const ws = balance?.windows ?? [];
    if (ws.length === 0) return ws;
    const order = (label: string) => {
      if (label.includes('5') || label.includes('小时') || label.includes('滚动')) return 0;
      if (label.includes('周')) return 1;
      if (label.includes('月')) return 2;
      return 3;
    };
    return [...ws].sort((a, b) => order(a.label) - order(b.label));
  }, [balance?.windows]);
  // 仅当存在 windows 时才按订阅样式渲染；zen 钱包等 windows 为空但 rows 有余额时走计费分支
  const hasWindows = sortedWindows.length > 0;
  const isSubscription = hasWindows && (balance?.provider === 'commandcode' || balance?.provider === 'opencode' || balance?.provider === 'opencode-go');
  if (unsupported) return null;

  const collapsedContent = (() => {
    if (hasData && balance!.ok) {
      if (sortedWindows.length > 0) {
        return (
          <div className="flex items-center flex-wrap gap-x-1 gap-y-0.5 min-w-0">
            {sortedWindows.map((w, idx) => (
              <React.Fragment key={w.label}>
                {idx > 0 && <span className="text-muted-foreground/30 mx-0.5">·</span>}
                <span className="font-mono font-medium text-xs text-foreground">
                  {w.label} <span className={cn(w.exceeded ? 'text-destructive' : 'text-muted-foreground')}>{Math.round(w.percent)}%</span>
                </span>
              </React.Fragment>
            ))}
          </div>
        );
      }
      // 钱包/计费类：windows 空但 summary 有余额（如 Zen $0.00）
      if (balance!.summary) {
        return <span className="font-mono text-success font-semibold text-xs truncate">{balance!.summary}</span>;
      }
      return <span className="text-muted-foreground/40 text-xs">{balance!.detail || ''}</span>;
    }
    if (hasData && !balance!.ok) {
      return <span className="text-warning truncate max-w-[16rem] text-xs" title={balance!.error}>{t_balanceError(balance!.error)}</span>;
    }
    if (!hasData && !loading) {
      return <span className="text-muted-foreground/40 text-xs">{unsupported ? '' : t_balanceHint()}</span>;
    }
    if (loading) {
      return <span className="text-muted-foreground/40 text-xs inline-flex items-center gap-1"><Loader2 size={12} className="animate-spin" /> 查询中…</span>;
    }
    return null;
  })();

  const hasRows = (balance?.rows?.length ?? 0) > 0;
  const canExpand = hasData && balance!.ok && (sortedWindows.length > 0 || hasRows);

  return (
    <div className="mt-2 rounded-lg border border-border/50 bg-muted/20 overflow-hidden">
      <div
        className={cn('flex items-center justify-between gap-2 px-2.5 py-2 min-w-0', canExpand && 'cursor-pointer hover:bg-muted/30 transition-colors')}
        onClick={() => { if (canExpand) setOpen(o => !o); }}
        role={canExpand ? 'button' : undefined}
        aria-expanded={canExpand ? open : undefined}
      >
        <div className="min-w-0 flex-1">{collapsedContent}</div>
        <div className="flex items-center gap-1 shrink-0">
          {canExpand && <ChevronDown size={14} className={cn('text-muted-foreground transition-transform duration-200', open && 'rotate-180')} />}
          {showBtn && (
            <button
              type="button"
              aria-label="Refresh balance"
              onClick={e => { e.stopPropagation(); onRefresh(); }}
              disabled={loading}
              className="p-1 rounded text-muted-foreground hover:text-foreground hover:bg-background transition-colors shrink-0 disabled:opacity-50"
            >
              {loading ? <Loader2 size={12} className="animate-spin" /> : <RefreshCw size={12} />}
            </button>
          )}
        </div>
      </div>
      {open && hasData && balance!.ok && (
        <div className="border-t border-border/50 bg-background/60 px-2.5 py-2 space-y-1.5 animate-in fade-in slide-in-from-top-1 duration-200">
          {isSubscription ? (
            sortedWindows.map(w => {
              const timeStr = w.resets_at ? formatAbsMs(w.resets_at) : '';
              const exhausted = !!w.exceeded;
              return (
                <div key={w.label} className="flex items-center justify-between gap-3 text-xs leading-relaxed min-w-0">
                  <span className="font-mono shrink-0 text-foreground">{w.label}</span>
                  {timeStr ? (
                    <span className={cn('font-mono text-xs truncate', exhausted ? 'text-destructive' : 'text-muted-foreground')}>
                      {exhausted ? `重置于 ${timeStr}` : `过期于 ${timeStr}`}
                    </span>
                  ) : <span className="font-mono text-xs text-muted-foreground/50 truncate">—</span>}
                </div>
              );
            })
          ) : (
            <>
              {sortedWindows.map(w => {
                const timeStr = w.resets_at ? formatAbsMs(w.resets_at) : '';
                const exhausted = !!w.exceeded;
                return (
                  <div key={w.label} className="flex items-center justify-between gap-3 text-xs leading-relaxed min-w-0">
                    <span className="font-mono text-foreground">{w.label}</span>
                    {timeStr ? (
                      <span className={cn('font-mono text-xs truncate', exhausted ? 'text-destructive' : 'text-muted-foreground')}>
                        {exhausted ? `重置于 ${timeStr}` : `过期于 ${timeStr}`}
                      </span>
                    ) : <span className="font-mono text-xs text-muted-foreground/50 truncate">—</span>}
                  </div>
                );
              })}
              {(balance!.rows ?? []).length > 0 && (
                <div className="flex flex-wrap gap-x-3 gap-y-1 pt-1 border-t border-border/30 text-xs text-muted-foreground">
                  {(balance!.rows ?? []).map(r => (
                    <span key={r.label} className="font-mono">{r.label} {r.value}</span>
                  ))}
                </div>
              )}
            </>
          )}
        </div>
      )}
    </div>
  );
}

function t_balanceError(err?: string): string {
  return err || '查询失败';
}
function t_balanceHint(): string {
  return '余额未查询';
}

function EndpointRow({ label, enabled, url, urls, onToggle, onChange }: { label: Protocol; enabled: boolean; url: string; urls: string[]; onToggle: (v: boolean) => void; onChange: (v: string) => void }) {
  const resolved = enabled ? resolvedEndpointUrl(url, label) : '';
  const [open, setOpen] = useState(false);
  // Sort suggestions by similarity to the current input: prefix match first,
  // then substring containment; ties keep the original order.
  const suggestions = useMemo(() => {
    const q = url.trim().toLowerCase();
    const score = (u: string) => {
      const s = u.toLowerCase();
      if (!q) return 0;
      if (s === q) return 3;
      if (s.startsWith(q)) return 2;
      if (s.includes(q)) return 1;
      return 0;
    };
    return urls
      .filter(u => !!u && u !== url)
      .map((u, idx) => ({ u, idx, score: score(u) }))
      .sort((a, b) => b.score - a.score || a.idx - b.idx)
      .map(x => x.u);
  }, [urls, url]);
  return (
    <div className="space-y-1.5">
      <label className="flex items-center gap-2 cursor-pointer min-w-0">
        <input type="checkbox" checked={enabled} onChange={e => onToggle(e.target.checked)} className="w-4 h-4 rounded accent-primary shrink-0" />
        <span className="text-xs font-bold uppercase">{label}</span>
        {resolved && (
          <span className="text-[10px] font-mono text-muted-foreground/70 truncate min-w-0">{resolved}</span>
        )}
      </label>
      {enabled && (
        <div className="relative">
          <div className="flex gap-2">
            <input
              value={url}
              onChange={e => onChange(e.target.value)}
              onFocus={() => setOpen(true)}
              onBlur={() => setTimeout(() => setOpen(false), 150)}
              placeholder="https://api.example.com/v1"
              className="flex-1 min-w-0 h-9 px-3 rounded-md border border-input bg-background text-sm font-mono"
            />
            <button
              type="button"
              tabIndex={-1}
              aria-label={`Select ${label} endpoint`}
              onClick={() => setOpen(v => !v)}
              className="shrink-0 h-9 px-2.5 rounded-md border border-input bg-background text-muted-foreground hover:text-foreground transition-colors"
            >
              <ChevronDown size={14} className={cn('transition-transform duration-200', open && 'rotate-180')} />
            </button>
          </div>
          {open && suggestions.length > 0 && (
            <div className="absolute z-20 mt-1 w-full max-h-44 overflow-y-auto rounded-md border border-border bg-popover text-popover-foreground shadow-lg">
              {suggestions.map(u => (
                <button
                  key={u}
                  type="button"
                  onMouseDown={e => e.preventDefault()}
                  onClick={() => { onChange(u); setOpen(false); }}
                  className="w-full text-left px-3 py-1.5 font-mono text-xs hover:bg-muted truncate"
                >
                  {u}
                </button>
              ))}
            </div>
          )}
        </div>
      )}
    </div>
  );
}

export default function Accounts() {
  const { t } = useTranslation();
  const { accounts, isLoading, keys, fetchAccounts, fetchAccountKey, addAccount, updateAccount, deleteAccount, toggleActive } = useAccountsStore();
  const { fetchModels, startTestQueue, availableModels } = useModelsStore();
  const [isModalOpen, setIsModalOpen] = useState(false);
  const [isEditOpen, setIsEditOpen] = useState(false);
  const [editingAccount, setEditingAccount] = useState<any>(null);
  const [formData, setFormData] = useState({ alias: '', api_key: '', chat_endpoint: '', responses_endpoint: '', messages_endpoint: '', default_protocol: '', balance_provider: '', balance_auth: '' });
  const [formEnabled, setFormEnabled] = useState<Record<Protocol, boolean>>({ chat: false, responses: false, messages: false });
  const [formShowKey, setFormShowKey] = useState(false);
  const [editData, setEditData] = useState({ alias: '', api_key: '', chat_endpoint: '', responses_endpoint: '', messages_endpoint: '', default_protocol: '', notes: '', balance_provider: '', balance_auth: '' });
  const [editEnabled, setEditEnabled] = useState<Record<Protocol, boolean>>({ chat: false, responses: false, messages: false });
  const [editShowKey, setEditShowKey] = useState(false);
  const [accountToDelete, setAccountToDelete] = useState<{ id: number; name: string } | null>(null);
  const [isValidating, setIsValidating] = useState(false);
  const [validationError, setValidationError] = useState<string | null>(null);
  const [visibleKeyId, setVisibleKeyId] = useState<number | null>(null);
  const [revealedKeys, setRevealedKeys] = useState<Record<number, string>>({});
  const [revealing, setRevealing] = useState<number | null>(null);
  // 账户列表搜索 + 状态筛选（循环：全部 → 禁用 → 启用）
  const [accountSearch, setAccountSearch] = useState('');
  const [accountFilter, setAccountFilter] = useState<'all' | 'disabled' | 'enabled'>('all');
  const FILTER_CYCLE = ['all', 'disabled', 'enabled'] as const;
  // Balance probe: per-account result + loading flag. Unsupported upstreams (422)
  // are remembered so the UI never re-probes them.
  const [balances, setBalances] = useState<Record<number, BalanceResult>>({});
  const [balanceLoading, setBalanceLoading] = useState<Record<number, boolean>>({});
  const [unsupportedBalances, setUnsupportedBalances] = useState<Set<number>>(new Set());

  const refreshBalance = async (id: number) => {
    setBalanceLoading(s => ({ ...s, [id]: true }));
    try {
      const res = await apiFetch(`/api/accounts/${id}/balance`);
      const data = await res.json().catch(() => ({}));
      if (res.ok) {
        setBalances(s => ({ ...s, [id]: data.balance }));
      } else {
        if (res.status === 422) {
          // 禁用查询：静默标记为不支持，不展示 422 错误文案
          setUnsupportedBalances(prev => new Set(prev).add(id));
          return;
        }
        setBalances(s => ({ ...s, [id]: { ok: false, error: data.message || `HTTP ${res.status}` } as BalanceResult }));
      }
    } catch (e: any) {
      setBalances(s => ({ ...s, [id]: { ok: false, error: e.message || 'network error' } as BalanceResult }));
    } finally {
      setBalanceLoading(s => ({ ...s, [id]: false }));
    }
  };

  // G: 打开页面即异步刷新全部余额（常显折叠态），不阻塞首屏；禁用查询的账户直接跳过不探针
  useEffect(() => {
    if (accounts.length === 0) return;
    accounts.forEach(acc => {
      if ((acc as any).balance_provider === 'none') {
        setUnsupportedBalances(prev => {
          if (prev.has(acc.id)) return prev;
          const n = new Set(prev); n.add(acc.id); return n;
        });
        return;
      }
      if (balances[acc.id] !== undefined) return;
      if (balanceLoading[acc.id]) return;
      if (unsupportedBalances.has(acc.id)) return;
      void refreshBalance(acc.id);
    });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [accounts.length]);

  const filteredAccounts = useMemo(() => {
    const q = accountSearch.trim().toLowerCase();
    return accounts.filter(acc => {
      if (accountFilter === 'disabled' && acc.is_active !== 0) return false;
      if (accountFilter === 'enabled' && acc.is_active === 0) return false;
      if (q && !acc.alias.toLowerCase().includes(q) && !(acc.provider_id || '').toLowerCase().includes(q)) return false;
      return true;
    });
  }, [accounts, accountSearch, accountFilter]);

  const toggleReveal = async (id: number) => {
    if (visibleKeyId === id) { setVisibleKeyId(null); return; }
    if (revealedKeys[id] || keys[id]) { setVisibleKeyId(id); return; }
    setRevealing(id);
    const k = await fetchAccountKey(id);
    setRevealing(null);
    if (k) setRevealedKeys(s => ({ ...s, [id]: k }));
    setVisibleKeyId(id);
  };

  useEffect(() => {
    fetchAccounts();
  }, []);

  // Local dedup of endpoint URLs across all accounts for datalist suggestions.
  const distinctUrls = useMemo(
    () => [...new Set(accounts.flatMap(a => [a.chat_endpoint, a.responses_endpoint, a.messages_endpoint].filter((u): u is string => !!u)))],
    [accounts]
  );

  // Edit mode suggests the endpoints this account already has configured on top
  // of the global list (the field itself may have been cleared in the form).
  const editUrlSuggestions = useMemo(() => {
    const own = editingAccount
      ? [editingAccount.base_url, editingAccount.anthropic_base_url, editingAccount.chat_endpoint, editingAccount.responses_endpoint, editingAccount.messages_endpoint].filter((u): u is string => !!u)
      : [];
    return [...new Set([...distinctUrls, ...own])];
  }, [distinctUrls, editingAccount]);

  const formEnabledProtocols = PROTOCOLS.filter(p => formEnabled[p]);
  const formError = formEnabledProtocols.length === 0
    ? t('accounts.needEndpoint', 'At least one endpoint must be enabled')
    : (!formData.default_protocol || !formEnabledProtocols.includes(formData.default_protocol as Protocol))
      ? t('accounts.defaultInEnabled', 'Default protocol must be one of the enabled endpoints')
      : null;

  const editEnabledProtocols = PROTOCOLS.filter(p => editEnabled[p]);
  const editError = editEnabledProtocols.length === 0
    ? t('accounts.needEndpoint', 'At least one endpoint must be enabled')
    : (!editData.default_protocol || !editEnabledProtocols.includes(editData.default_protocol as Protocol))
      ? t('accounts.defaultInEnabled', 'Default protocol must be one of the enabled endpoints')
      : null;

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (formError) {
      setValidationError(formError);
      return;
    }
    setIsValidating(true);
    setValidationError(null);
    try {
      await addAccount({
        alias: formData.alias,
        provider_id: 'custom',
        api_key: formData.api_key,
        chat_endpoint: formEnabled.chat ? formData.chat_endpoint.trim() || null : null,
        responses_endpoint: formEnabled.responses ? formData.responses_endpoint.trim() || null : null,
        messages_endpoint: formEnabled.messages ? formData.messages_endpoint.trim() || null : null,
        default_protocol: formData.default_protocol,
        balance_provider: formData.balance_provider,
        balance_auth: COOKIE_KINDS.includes(formData.balance_provider) ? formData.balance_auth : '',
        openai_compatible: 0,
      });
      setIsModalOpen(false);
      setFormData({ alias: '', api_key: '', chat_endpoint: '', responses_endpoint: '', messages_endpoint: '', default_protocol: '', balance_provider: '', balance_auth: '' });
      setFormEnabled({ chat: false, responses: false, messages: false });
      setFormShowKey(false);
    } catch (err: any) {
      setValidationError(err.message || "Validation failed");
    } finally {
      setIsValidating(false);
    }
  };


  const openEdit = (acc: any) => {
    setEditingAccount(acc);
    const enabled: Record<Protocol, boolean> = {
      chat: !!acc.chat_endpoint,
      responses: !!acc.responses_endpoint,
      messages: !!acc.messages_endpoint,
    };
    setEditEnabled(enabled);
    setEditData({
      alias: acc.alias,
      api_key: '',
      chat_endpoint: acc.chat_endpoint || '',
      responses_endpoint: acc.responses_endpoint || '',
      messages_endpoint: acc.messages_endpoint || '',
      default_protocol: acc.default_protocol || '',
      notes: acc.notes || '',
      balance_provider: acc.balance_provider || '',
      balance_auth: '',
    });
    setIsEditOpen(true);
  };

  const handleEditSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (editError) {
      setValidationError(editError);
      return;
    }
    if (!editingAccount) return;
    setIsValidating(true);
    setValidationError(null);
    try {
      const payload: any = {
        alias: editData.alias,
        provider_id: editingAccount.provider_id,
        notes: editData.notes,
        chat_endpoint: editEnabled.chat ? editData.chat_endpoint.trim() || null : null,
        responses_endpoint: editEnabled.responses ? editData.responses_endpoint.trim() || null : null,
        messages_endpoint: editEnabled.messages ? editData.messages_endpoint.trim() || null : null,
        default_protocol: editData.default_protocol,
        balance_provider: editData.balance_provider,
      };
      // 凭据只在填入且类型匹配时提交；留空 = 不修改已有值。
      if (editData.balance_auth && COOKIE_KINDS.includes(editData.balance_provider)) payload.balance_auth = editData.balance_auth;
      if (editData.api_key) payload.api_key = editData.api_key;
      await updateAccount(editingAccount.id, payload);
      // 余额配置可能变了：清掉旧的"不支持"标记与旧结果，让刷新按钮重新出现
      const updatedId = editingAccount.id;
      setUnsupportedBalances(prev => { const n = new Set(prev); n.delete(updatedId); return n; });
      setBalances(s => { const n = { ...s }; delete n[updatedId]; return n; });
      setIsEditOpen(false);
      setEditingAccount(null);
    } catch (err: any) {
      setValidationError(err.message || "Update validation failed");
    } finally {
      setIsValidating(false);
    }
  };

  const getSyncScript = () => {
    return `(async()=>{const p="custom";console.log("🚀 LLMux Syncing...");const t=localStorage.getItem("token")||document.cookie;fetch("http://localhost:25975/api/auth/sync",{method:"POST",body:JSON.stringify({provider:p,token:t})})})();`;
  };

  const handleToggle = (proto: Protocol, v: boolean) => {
    setFormEnabled(prev => ({ ...prev, [proto]: v }));
    // Auto-pick default when turning on the first endpoint or when current default no longer valid.
    setFormData(prev => {
      if (v && !prev.default_protocol) return { ...prev, default_protocol: proto };
      return prev;
    });
  };

  const handleEditToggle = (proto: Protocol, v: boolean) => {
    setEditEnabled(prev => ({ ...prev, [proto]: v }));
    setEditData(prev => {
      if (v && !prev.default_protocol) return { ...prev, default_protocol: proto };
      return prev;
    });
  };

  return (
    <div className="space-y-10 animate-fadeIn">
      <div className="flex items-center justify-between">
        <div className="flex items-start gap-3">
          <div className="p-2 bg-primary/10 text-primary rounded-lg mt-1.5">
            <Users size={24} />
          </div>
          <div>
            <h1 className="text-2xl font-semibold tracking-tight">{t('common.accounts')}</h1>
            <p className="text-sm text-muted-foreground">{t('accounts.subtitle')}</p>
          </div>
        </div>
        <Button
          onClick={() => setIsModalOpen(true)}
          size="sm"
        >
          <Plus size={16} />
          {t('accounts.addAccount')}
        </Button>
      </div>

      {/* 搜索 + 禁用筛选 */}
      <div className="flex items-center gap-2 justify-between">
        <div className="relative flex-1 max-w-sm">
          {accountSearch === '' && (
            <Search className="absolute left-3 top-1/2 -translate-y-1/2 text-muted-foreground z-10" size={14} />
          )}
          <Input
            type="text"
            placeholder={t('accounts.filter.searchPlaceholder')}
            value={accountSearch}
            onChange={e => setAccountSearch(e.target.value)}
            className={accountSearch ? 'pl-3' : 'pl-9'}
          />
        </div>
        <Button
          variant="outline"
          size="sm"
          onClick={() => setAccountFilter(FILTER_CYCLE[(FILTER_CYCLE.indexOf(accountFilter) + 1) % FILTER_CYCLE.length])}
          className={accountFilter !== 'all' ? 'text-primary border-primary/40' : ''}
        >
          <Filter size={14} />
          {t(`accounts.filter.${accountFilter}`)}
        </Button>
      </div>

      {isLoading && (
        <div className="py-20 flex justify-center">
          <Loader2 className="animate-spin text-primary/50" />
        </div>
      )}

      <div className="space-y-3">
        {filteredAccounts.map((acc) => (
          <div key={acc.id} className="p-4 rounded-xl border border-border bg-card hover:bg-muted/30 transition-all group">
            <div className="flex items-center justify-between gap-4">
              <div className="flex items-center gap-2 min-w-0">
                <h3 className="font-semibold text-sm truncate">{acc.alias}</h3>
                <StatusDot status={acc.is_active === 1 ? 'online' : 'offline'} />
              </div>

              <div className="flex items-center gap-2 shrink-0">
                  <Button
                    variant="ghost"
                    size="icon"
                    onClick={() => openEdit(acc)}
                    className="text-warning hover:text-warning hover:bg-warning/10"
                    title="Edit account"
                  >
                    <Pencil size={16} />
                  </Button>
                  <Button
                    variant="ghost"
                    size="icon"
                    onClick={() => toggleActive(acc.id, acc.is_active)}
                    className={acc.is_active === 1 ? "text-success hover:text-success hover:bg-success/10" : "text-muted-foreground/40 hover:text-muted-foreground hover:bg-muted"}
                    title={acc.is_active === 1 ? t('common.online') : t('accounts.offline')}
                  >
                    <Power size={16} />
                  </Button>
                  <Button
                    variant="ghost"
                    size="icon"
                    onClick={() => setAccountToDelete({ id: acc.id, name: acc.alias })}
                    className="text-destructive hover:text-destructive hover:bg-destructive/10"
                  >
                    <Trash2 size={16} />
                  </Button>
                </div>
            </div>
            {/* API key 独立末行：明文换行显示，不把按钮顶走 */}
            <div className="text-xs text-muted-foreground mt-2 flex items-start gap-1.5 uppercase tracking-tight">
              <Key size={10} className="mt-0.5 shrink-0" /> {t('accounts.apiKey')}: {visibleKeyId === acc.id ? (revealing === acc.id ? <span className="font-mono normal-case lowercase text-muted-foreground/50">…</span> : ((revealedKeys[acc.id] ?? keys[acc.id]) ? <span className="font-mono normal-case lowercase break-all min-w-0">{revealedKeys[acc.id] ?? keys[acc.id]}</span> : <span className="text-muted-foreground/50">—</span>)) : '****'}
              <button
                type="button"
                aria-label={visibleKeyId === acc.id ? 'Hide API key' : 'Show API key'}
                onClick={() => toggleReveal(acc.id)}
                disabled={revealing === acc.id}
                className="p-0.5 rounded text-muted-foreground hover:text-foreground hover:bg-muted transition-colors shrink-0 disabled:opacity-50"
              >
                {visibleKeyId === acc.id ? <EyeOff size={12} /> : <Eye size={12} />}
              </button>
            </div>
            {/* 余额/用量行：手动刷新（CodexBar 移植），不支持的上游隐藏按钮 */}
            <BalanceLine
              account={acc}
              balance={balances[acc.id]}
              loading={balanceLoading[acc.id] ?? false}
              unsupported={unsupportedBalances.has(acc.id)}
              onRefresh={() => refreshBalance(acc.id)}
            />
          </div>
        ))}

        {!isLoading && accounts.length > 0 && filteredAccounts.length === 0 && (
          <div className="py-10 text-center border border-dashed border-border rounded-xl">
             <p className="text-sm text-muted-foreground">{t('accounts.noMatch')}</p>
          </div>
        )}

        {!isLoading && accounts.length === 0 && (
          <div className="py-20 text-center border-2 border-dashed border-border rounded-xl">
             <AlertCircle className="mx-auto mb-2 text-muted-foreground/30" />
             <p className="text-sm text-muted-foreground">{t('accounts.noAccounts')}</p>
          </div>
        )}
      </div>

      <Dialog
        isOpen={isModalOpen}
        onClose={() => !isValidating && setIsModalOpen(false)}
        title={t('accounts.registerTitle')}
        size="lg"
      >
        <div className="space-y-6">
          {validationError && (
            <div className="p-3 bg-destructive/10 border border-destructive/20 rounded-lg flex items-start gap-2 text-destructive text-xs animate-in slide-in-from-top-2">
              <AlertCircle size={14} className="shrink-0 mt-0.5" />
              <p className="font-medium">{t(validationError)}</p>
            </div>
          )}

          <form onSubmit={handleSubmit} className="space-y-4">
            <div className="space-y-1.5">
              <label className="text-xs font-bold text-muted-foreground uppercase">{t('accounts.alias')}</label>
              <Input
                type="text" required value={formData.alias}
                disabled={isValidating}
                onChange={e => setFormData({ ...formData, alias: e.target.value })}
                placeholder={t('accounts.aliasPlaceholder')}
              />
            </div>
            <div className="space-y-1.5">
              <label className="text-xs font-bold text-muted-foreground uppercase">{t('accounts.apiKey')}</label>
              <div className="relative">
                <Input
                  type={formShowKey ? 'text' : 'password'} required value={formData.api_key}
                  disabled={isValidating}
                  onChange={e => setFormData({ ...formData, api_key: e.target.value })}
                  placeholder="sk-..."
                  className="font-mono pr-10"
                />
                <button
                  type="button"
                  tabIndex={-1}
                  aria-label={formShowKey ? 'Hide API key' : 'Show API key'}
                  onClick={() => setFormShowKey(v => !v)}
                  className="absolute right-2 top-1/2 -translate-y-1/2 p-1 text-muted-foreground hover:text-foreground"
                >
                  {formShowKey ? <EyeOff size={14} /> : <Eye size={14} />}
                </button>
              </div>
            </div>

            <div className="space-y-3 border-t border-border pt-3">
              <label className="text-xs font-bold text-muted-foreground uppercase">{t('accounts.endpoints', 'Endpoints')}</label>
              <EndpointRow
                label="chat"
                enabled={formEnabled.chat}
                url={formData.chat_endpoint}
                urls={distinctUrls}
                onToggle={v => handleToggle('chat', v)}
                onChange={v => setFormData({ ...formData, chat_endpoint: v })}
              />
              <EndpointRow
                label="responses"
                enabled={formEnabled.responses}
                url={formData.responses_endpoint}
                urls={distinctUrls}
                onToggle={v => handleToggle('responses', v)}
                onChange={v => setFormData({ ...formData, responses_endpoint: v })}
              />
              <EndpointRow
                label="messages"
                enabled={formEnabled.messages}
                url={formData.messages_endpoint}
                urls={distinctUrls}
                onToggle={v => handleToggle('messages', v)}
                onChange={v => setFormData({ ...formData, messages_endpoint: v })}
              />
              {formError && (
                <p className="text-xs font-medium text-destructive">{formError}</p>
              )}
            </div>

            <div className="space-y-1.5">
              <label className="text-xs font-bold text-muted-foreground uppercase">{t('accounts.defaultProtocol', 'Default protocol')}</label>
              <ToggleGroup
                type="single"
                variant="outline"
                value={formData.default_protocol}
                disabled={formEnabledProtocols.length === 0 || isValidating}
                onValueChange={v => { if (v) setFormData({ ...formData, default_protocol: v }); }}
                className="justify-start flex-wrap"
              >
                {formEnabledProtocols.map(p => (
                  <ToggleGroupItem key={p} value={p} className="capitalize">{p}</ToggleGroupItem>
                ))}
              </ToggleGroup>
              <p className="text-xs text-muted-foreground">{t('accounts.defaultProtocolHint', 'Used when a request does not specify a protocol')}</p>
            </div>

            <div className="space-y-1.5">
              <label className="text-xs font-bold text-muted-foreground uppercase">{t('accounts.balanceProvider', '余额查询接口')}</label>
              <select
                value={formData.balance_provider}
                disabled={isValidating}
                onChange={e => setFormData({ ...formData, balance_provider: e.target.value })}
                className="w-full h-10 px-3 py-2 rounded-md border border-input bg-background text-sm ring-offset-background focus:outline-none focus:ring-2 focus:ring-ring focus:ring-offset-2"
              >
                <option value="">{t('accounts.balanceAuto', '自动检测')}</option>
                <option value="deepseek">DeepSeek</option>
                <option value="copilot">Copilot</option>
                <option value="openrouter">OpenRouter</option>
                <option value="commandcode">CommandCode</option>
                <option value="opencode">OpenCode</option>
                <option value="opencode-go">OpenCode Go</option>
                <option value="opencode-zen">OpenCode Zen</option>
                <option value="none">{t('accounts.balanceDisabled', '禁用查询')}</option>
              </select>
              <p className="text-xs text-muted-foreground">{t('accounts.balanceProviderHint', '决定余额查询走哪个上游接口；自动检测按名称/地址推断')}</p>
              {COOKIE_KINDS.includes(formData.balance_provider) && (
                <BalanceAuthInput
                  kind={formData.balance_provider}
                  value={formData.balance_auth}
                  onChange={v => setFormData({ ...formData, balance_auth: v })}
                  disabled={isValidating}
                />
              )}
            </div>

            <div className="pt-4 flex gap-3">
               <Button
                 type="button"
                 variant="outline"
                 disabled={isValidating}
                 onClick={() => setIsModalOpen(false)}
                 className="flex-1"
               >
                 {t('common.cancel')}
               </Button>
               <Button
                 type="submit"
                 disabled={isValidating || !!formError}
                 className="flex-1"
               >
                 {isValidating ? (
                   <>
                     <Loader2 size={16} className="animate-spin" />
                     {t('accounts.validating', '验证中...')}
                   </>
                 ) : (
                   <>
                     <Save size={16} />
                     {t('common.save')}
                   </>
                 )}
               </Button>
            </div>
          </form>
        </div>
      </Dialog>

      {/* 编辑账户 Modal */}
      <Dialog
        isOpen={isEditOpen}
        onClose={() => !isValidating && setIsEditOpen(false)}
        title={t('accounts.editAccount')}
        size="lg"
      >
        <div className="space-y-6">
          {validationError && (
            <div className="p-3 bg-destructive/10 border border-destructive/20 rounded-lg flex items-start gap-2 text-destructive text-xs animate-in slide-in-from-top-2">
              <AlertCircle size={14} className="shrink-0 mt-0.5" />
              <p className="font-medium">{t(validationError)}</p>
            </div>
          )}

          <form onSubmit={handleEditSubmit} className="space-y-4">
            <div className="space-y-1.5">
              <label className="text-xs font-bold text-muted-foreground uppercase">{t('accounts.alias')}</label>
              <Input
                type="text" required value={editData.alias}
                disabled={isValidating}
                onChange={e => setEditData({ ...editData, alias: e.target.value })}
              />
            </div>
            <div className="space-y-1.5">
              <div className="flex items-center justify-between">
                <label className="text-xs font-bold text-muted-foreground uppercase">API Key</label>
                <span className="text-xs text-muted-foreground italic">{t('accounts.leaveBlank')}</span>
              </div>
              <div className="relative">
                <Input
                  type={editShowKey ? 'text' : 'password'} value={editData.api_key}
                  disabled={isValidating}
                  onChange={e => setEditData({ ...editData, api_key: e.target.value })}
                  placeholder={t('accounts.leaveBlank')}
                  className="font-mono pr-10"
                />
                <button
                  type="button"
                  tabIndex={-1}
                  aria-label={editShowKey ? 'Hide API key' : 'Show API key'}
                  onClick={() => setEditShowKey(v => !v)}
                  className="absolute right-2 top-1/2 -translate-y-1/2 p-1 text-muted-foreground hover:text-foreground"
                >
                  {editShowKey ? <EyeOff size={14} /> : <Eye size={14} />}
                </button>
              </div>
            </div>

            <div className="space-y-3 border-t border-border pt-3">
              <label className="text-xs font-bold text-muted-foreground uppercase">{t('accounts.endpoints', 'Endpoints')}</label>
              <EndpointRow
                label="chat"
                enabled={editEnabled.chat}
                url={editData.chat_endpoint}
                urls={editUrlSuggestions}
                onToggle={v => handleEditToggle('chat', v)}
                onChange={v => setEditData({ ...editData, chat_endpoint: v })}
              />
              <EndpointRow
                label="responses"
                enabled={editEnabled.responses}
                url={editData.responses_endpoint}
                urls={editUrlSuggestions}
                onToggle={v => handleEditToggle('responses', v)}
                onChange={v => setEditData({ ...editData, responses_endpoint: v })}
              />
              <EndpointRow
                label="messages"
                enabled={editEnabled.messages}
                url={editData.messages_endpoint}
                urls={editUrlSuggestions}
                onToggle={v => handleEditToggle('messages', v)}
                onChange={v => setEditData({ ...editData, messages_endpoint: v })}
              />
              {editError && (
                <p className="text-xs font-medium text-destructive">{editError}</p>
              )}
            </div>

            <div className="space-y-1.5">
              <label className="text-xs font-bold text-muted-foreground uppercase">{t('accounts.defaultProtocol', 'Default protocol')}</label>
              <ToggleGroup
                type="single"
                variant="outline"
                value={editData.default_protocol}
                disabled={editEnabledProtocols.length === 0 || isValidating}
                onValueChange={v => { if (v) setEditData({ ...editData, default_protocol: v }); }}
                className="justify-start flex-wrap"
              >
                {editEnabledProtocols.map(p => (
                  <ToggleGroupItem key={p} value={p} className="capitalize">{p}</ToggleGroupItem>
                ))}
              </ToggleGroup>
              <p className="text-xs text-muted-foreground">{t('accounts.defaultProtocolHint', 'Used when a request does not specify a protocol')}</p>
            </div>

            <div className="space-y-1.5">
              <label className="text-xs font-bold text-muted-foreground uppercase">{t('accounts.balanceProvider', '余额查询接口')}</label>
              <select
                value={editData.balance_provider}
                disabled={isValidating}
                onChange={e => setEditData({ ...editData, balance_provider: e.target.value })}
                className="w-full h-10 px-3 py-2 rounded-md border border-input bg-background text-sm ring-offset-background focus:outline-none focus:ring-2 focus:ring-ring focus:ring-offset-2"
              >
                <option value="">{t('accounts.balanceAuto', '自动检测')}</option>
                <option value="deepseek">DeepSeek</option>
                <option value="copilot">Copilot</option>
                <option value="openrouter">OpenRouter</option>
                <option value="commandcode">CommandCode</option>
                <option value="opencode">OpenCode</option>
                <option value="opencode-go">OpenCode Go</option>
                <option value="opencode-zen">OpenCode Zen</option>
                <option value="none">{t('accounts.balanceDisabled', '禁用查询')}</option>
              </select>
              <p className="text-xs text-muted-foreground">{t('accounts.balanceProviderHint', '决定余额查询走哪个上游接口；自动检测按名称/地址推断')}</p>
              {COOKIE_KINDS.includes(editData.balance_provider) && (
                <BalanceAuthInput
                  kind={editData.balance_provider}
                  value={editData.balance_auth}
                  onChange={v => setEditData({ ...editData, balance_auth: v })}
                  disabled={isValidating}
                />
              )}
            </div>

            <div className="pt-4 flex gap-3">
               <Button
                 type="button"
                 variant="outline"
                 disabled={isValidating}
                 onClick={() => { setIsEditOpen(false); setEditingAccount(null); }}
                 className="flex-1"
               >
                 {t('common.cancel')}
               </Button>
               <Button
                 type="submit"
                 disabled={isValidating || !!editError}
                 className="flex-1"
               >
                 {isValidating ? (
                   <>
                     <Loader2 size={16} className="animate-spin" />
                     {t('accounts.validating', '验证中...')}
                   </>
                 ) : (
                   <>
                     <Save size={16} />
                     {t('common.save')}
                   </>
                 )}
               </Button>
            </div>
          </form>
        </div>
      </Dialog>

      {/* 增强型删除确认弹窗 */}
      <Dialog
        isOpen={!!accountToDelete}
        onClose={() => setAccountToDelete(null)}
        title={t('common.delete')}
        variant="danger"
        size="md"
        footer={
          <div className="flex items-center justify-end w-full">
            <div className="flex items-center gap-3">
              <Button variant="outline" size="sm" onClick={() => setAccountToDelete(null)}>
                {t('common.cancel')}
              </Button>
              <Button
                variant="destructive"
                size="sm"
                onClick={async () => {
                  if (accountToDelete) {
                    await deleteAccount(accountToDelete.id);
                    setAccountToDelete(null);
                  }
                }}
              >
                <Trash2 size={16} />
                {t('common.delete')}
              </Button>
            </div>
          </div>
        }
      >
        <div className="space-y-4">
           <div className="p-4 bg-destructive/5 border border-destructive/10 rounded-xl flex gap-4">
              <ShieldAlert size={24} className="text-destructive shrink-0" />
              <div className="space-y-1">
                 <p className="text-sm font-semibold text-destructive">{t('accounts.deleteWarning')}</p>
                 <p className="text-xs text-destructive/80 leading-relaxed">
                   {t('accounts.deleteConfirm', { name: accountToDelete?.name })}
                 </p>
              </div>
           </div>
           <p className="text-xs text-muted-foreground px-1 italic">
             {t('accounts.deleteWarningDetail')}
           </p>
        </div>
      </Dialog>
    </div>
  );
}
