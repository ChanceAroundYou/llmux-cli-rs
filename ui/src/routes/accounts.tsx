import React, { useEffect, useMemo, useState } from 'react';
import { useAccountsStore } from '../stores/accounts';
import { useModelsStore } from '../stores/models';
import {
  Users,
  Trash2,
  Plus,
  Settings2,
  Key,
  Globe,
  Loader2,
  AlertCircle,
  Save,
  Monitor,
  Copy,
  CheckCircle2,
  Pencil,
  ShieldAlert,
  Power
} from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { Dialog, ConfirmDialog } from '../components/Modal';
import { CopyButton } from '../components/CopyButton';
import { cn } from '../lib/utils';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { StatusBadge } from '@/components/shared/StatusBadge';
import { ToggleGroup, ToggleGroupItem } from '@/components/ui/toggle-group';

const PROTOCOLS = ['chat', 'responses', 'messages'] as const;
type Protocol = typeof PROTOCOLS[number];

const PROTOCOL_LABEL: Record<Protocol, string> = {
  chat: '/v1/chat/completions',
  responses: '/v1/responses',
  messages: '/v1/messages',
};

function ProtocolBadge({ proto }: { proto: Protocol }) {
  return (
    <span className="inline-flex items-center rounded-md bg-muted px-2 py-0.5 text-xs font-medium uppercase tracking-tight border border-border">
      {proto}
    </span>
  );
}

function EndpointRow({ label, enabled, url, urls, onToggle, onChange }: { label: Protocol; enabled: boolean; url: string; urls: string[]; onToggle: (v: boolean) => void; onChange: (v: string) => void }) {
  return (
    <div className="space-y-1.5">
      <label className="flex items-center gap-2 cursor-pointer">
        <input type="checkbox" checked={enabled} onChange={e => onToggle(e.target.checked)} className="w-4 h-4 rounded accent-primary" />
        <span className="text-xs font-bold uppercase">{label}</span>
        <span className="text-[10px] font-mono text-muted-foreground/70">{PROTOCOL_LABEL[label]}</span>
      </label>
      {enabled && (
        <div className="flex gap-2">
          <input list={`${label}-urls`} value={url} onChange={e => onChange(e.target.value)} placeholder="https://api.example.com/v1" className="flex-1 h-9 px-3 rounded-md border border-input bg-background text-sm font-mono" />
          <datalist id={`${label}-urls`}>{urls.map(u => <option key={u} value={u} />)}</datalist>
        </div>
      )}
    </div>
  );
}

export default function Accounts() {
  const { t } = useTranslation();
  const { accounts, isLoading, fetchAccounts, addAccount, updateAccount, deleteAccount, toggleActive } = useAccountsStore();
  const { fetchModels, startTestQueue, availableModels } = useModelsStore();
  const [isModalOpen, setIsModalOpen] = useState(false);
  const [isEditOpen, setIsEditOpen] = useState(false);
  const [editingAccount, setEditingAccount] = useState<any>(null);
  const [formData, setFormData] = useState({ alias: '', api_key: '', chat_endpoint: '', responses_endpoint: '', messages_endpoint: '', default_protocol: '' });
  const [formEnabled, setFormEnabled] = useState<Record<Protocol, boolean>>({ chat: false, responses: false, messages: false });
  const [formSkipValidation, setFormSkipValidation] = useState(false);
  const [editData, setEditData] = useState({ alias: '', api_key: '', chat_endpoint: '', responses_endpoint: '', messages_endpoint: '', default_protocol: '', notes: '' });
  const [editEnabled, setEditEnabled] = useState<Record<Protocol, boolean>>({ chat: false, responses: false, messages: false });
  const [editSkipValidation, setEditSkipValidation] = useState(false);
  const [accountToDelete, setAccountToDelete] = useState<{ id: number; name: string } | null>(null);
  const [isValidating, setIsValidating] = useState(false);
  const [validationError, setValidationError] = useState<string | null>(null);

  useEffect(() => {
    fetchAccounts();
  }, []);

  // Local dedup of endpoint URLs across all accounts for datalist suggestions.
  const distinctUrls = useMemo(
    () => [...new Set(accounts.flatMap(a => [a.chat_endpoint, a.responses_endpoint, a.messages_endpoint].filter((u): u is string => !!u)))],
    [accounts]
  );

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
        skip_validation: formSkipValidation,
      });
      setIsModalOpen(false);
      setFormData({ alias: '', api_key: '', chat_endpoint: '', responses_endpoint: '', messages_endpoint: '', default_protocol: '' });
      setFormEnabled({ chat: false, responses: false, messages: false });
      setFormSkipValidation(false);
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
        skip_validation: editSkipValidation,
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

      {isLoading && (
        <div className="py-20 flex justify-center">
          <Loader2 className="animate-spin text-primary/50" />
        </div>
      )}

      <div className="space-y-3">
        {accounts.map((acc) => (
          <div key={acc.id} className="p-4 rounded-xl border border-border bg-card hover:bg-muted/30 transition-all flex items-center justify-between group">
            <div className="flex items-center gap-4">
              <div className="w-10 h-10 rounded-lg bg-muted flex items-center justify-center font-bold text-xs uppercase border border-border">
                {acc.provider_id.slice(0, 2)}
              </div>
              <div>
                <div className="flex items-center gap-2">
                   <h3 className="font-semibold text-sm">{acc.alias}</h3>
                   <StatusBadge status={acc.is_active === 1 ? 'online' : 'offline'} label={acc.is_active === 1 ? t('common.online') : t('accounts.offline')} />
                </div>
                <div className="text-xs text-muted-foreground mt-0.5 flex items-center gap-2 uppercase tracking-tight">
                  <Globe size={10} /> {acc.provider_id}
                  <span className="opacity-20">|</span>
                  <Key size={10} /> {t('accounts.apiKey')}: ****
                </div>
              </div>
            </div>

              <div className="flex items-center gap-2 opacity-0 group-hover:opacity-100 transition-opacity">
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
        ))}

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
              <Input
                type="password" required value={formData.api_key}
                disabled={isValidating}
                onChange={e => setFormData({ ...formData, api_key: e.target.value })}
                placeholder="sk-..."
                className="font-mono"
              />
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

            <div className="space-y-1.5 border-t border-border pt-3">
              <label className="flex items-center gap-2 cursor-pointer select-none">
                <input
                  type="checkbox"
                  checked={formSkipValidation}
                  disabled={isValidating}
                  onChange={e => setFormSkipValidation(e.target.checked)}
                  className="w-4 h-4 rounded accent-primary"
                />
                <span className="text-xs font-bold text-muted-foreground uppercase">{t('accounts.skipValidation')}</span>
              </label>
              <p className="text-xs text-muted-foreground ml-6">{t('accounts.skipValidationHint')}</p>
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
              <label className="text-xs font-bold text-muted-foreground uppercase">{t('accounts.provider')}</label>
              <div className="flex items-center gap-2 h-10 px-3 rounded-md border border-input bg-muted/40 text-sm">
                <ProtocolBadge proto={editingAccount?.provider_id} />
                <span className="text-xs text-muted-foreground italic">{t('accounts.readOnly', 'Read-only')}</span>
              </div>
            </div>
            <div className="space-y-1.5">
              <div className="flex items-center justify-between">
                <label className="text-xs font-bold text-muted-foreground uppercase">API Key</label>
                <span className="text-xs text-muted-foreground italic">{t('accounts.leaveBlank')}</span>
              </div>
              <Input
                type="password" value={editData.api_key}
                disabled={isValidating}
                onChange={e => setEditData({ ...editData, api_key: e.target.value })}
                placeholder={t('accounts.leaveBlank')}
                className="font-mono"
              />
            </div>

            <div className="space-y-3 border-t border-border pt-3">
              <label className="text-xs font-bold text-muted-foreground uppercase">{t('accounts.endpoints', 'Endpoints')}</label>
              <EndpointRow
                label="chat"
                enabled={editEnabled.chat}
                url={editData.chat_endpoint}
                urls={distinctUrls}
                onToggle={v => handleEditToggle('chat', v)}
                onChange={v => setEditData({ ...editData, chat_endpoint: v })}
              />
              <EndpointRow
                label="responses"
                enabled={editEnabled.responses}
                url={editData.responses_endpoint}
                urls={distinctUrls}
                onToggle={v => handleEditToggle('responses', v)}
                onChange={v => setEditData({ ...editData, responses_endpoint: v })}
              />
              <EndpointRow
                label="messages"
                enabled={editEnabled.messages}
                url={editData.messages_endpoint}
                urls={distinctUrls}
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

            <div className="space-y-1.5 border-t border-border pt-3">
              <label className="flex items-center gap-2 cursor-pointer select-none">
                <input
                  type="checkbox"
                  checked={editSkipValidation}
                  disabled={isValidating}
                  onChange={e => setEditSkipValidation(e.target.checked)}
                  className="w-4 h-4 rounded accent-primary"
                />
                <span className="text-xs font-bold text-muted-foreground uppercase">{t('accounts.skipValidation')}</span>
              </label>
              <p className="text-xs text-muted-foreground ml-6">{t('accounts.skipValidationHint')}</p>
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
