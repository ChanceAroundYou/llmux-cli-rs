import React, { useEffect, useMemo, useState } from 'react';
import { useAccountsStore } from '../stores/accounts';
import { useModelsStore } from '../stores/models';
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
      <label className="flex items-center gap-2 cursor-pointer">
        <input type="checkbox" checked={enabled} onChange={e => onToggle(e.target.checked)} className="w-4 h-4 rounded accent-primary" />
        <span className="text-xs font-bold uppercase">{label}</span>
        {resolved && (
          <span className="text-[10px] font-mono text-muted-foreground/70">{resolved}</span>
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
              className="flex-1 h-9 px-3 rounded-md border border-input bg-background text-sm font-mono"
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
  const [formData, setFormData] = useState({ alias: '', api_key: '', chat_endpoint: '', responses_endpoint: '', messages_endpoint: '', default_protocol: '' });
  const [formEnabled, setFormEnabled] = useState<Record<Protocol, boolean>>({ chat: false, responses: false, messages: false });
  const [formShowKey, setFormShowKey] = useState(false);
  const [editData, setEditData] = useState({ alias: '', api_key: '', chat_endpoint: '', responses_endpoint: '', messages_endpoint: '', default_protocol: '', notes: '' });
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
        openai_compatible: 0,
      });
      setIsModalOpen(false);
      setFormData({ alias: '', api_key: '', chat_endpoint: '', responses_endpoint: '', messages_endpoint: '', default_protocol: '' });
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
      };
      if (editData.api_key) payload.api_key = editData.api_key;
      await updateAccount(editingAccount.id, payload);
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
