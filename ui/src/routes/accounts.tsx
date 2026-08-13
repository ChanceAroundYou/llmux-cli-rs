import React, { useEffect, useState } from 'react';
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

export default function Accounts() {
  const { t } = useTranslation();
  const { accounts, isLoading, fetchAccounts, addAccount, updateAccount, deleteAccount, toggleActive } = useAccountsStore();
  const { fetchModels, startTestQueue, availableModels } = useModelsStore();
  const [isModalOpen, setIsModalOpen] = useState(false);
  const [isEditOpen, setIsEditOpen] = useState(false);
  const [editingAccount, setEditingAccount] = useState<any>(null);
  const [formData, setFormData] = useState({ alias: '', provider_id: 'custom', api_key: '', base_url: '', anthropic_base_url: '' });
  const [formSupportsAnthropic, setFormSupportsAnthropic] = useState(false);
  const [formOpenAICompat, setFormOpenAICompat] = useState(false);
  const [formSkipValidation, setFormSkipValidation] = useState(false);
  const [editData, setEditData] = useState({ alias: '', provider_id: '', api_key: '', base_url: '', anthropic_base_url: '', notes: '' });
  const [editSupportsAnthropic, setEditSupportsAnthropic] = useState(false);
  const [editOpenAICompat, setEditOpenAICompat] = useState(false);
  const [editSkipValidation, setEditSkipValidation] = useState(false);
  const [accountToDelete, setAccountToDelete] = useState<{id: number, name: string} | null>(null);
  const [isValidating, setIsValidating] = useState(false);
  const [validationError, setValidationError] = useState<string | null>(null);

  useEffect(() => {
    fetchAccounts();
  }, []);

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    setIsValidating(true);
    setValidationError(null);
    try {
      await addAccount({ ...formData, openai_compatible: formOpenAICompat ? 1 : 0, skip_validation: formSkipValidation });
      setIsModalOpen(false);
      setFormData({ alias: '', provider_id: 'custom', api_key: '', base_url: '', anthropic_base_url: '' });
      setFormSupportsAnthropic(false);
      setFormOpenAICompat(false);
      setFormSkipValidation(false);
    } catch (err: any) {
      setValidationError(err.message || "Validation failed");
    } finally {
      setIsValidating(false);
    }
  };


  const openEdit = (acc: any) => {
    setEditingAccount(acc);
    setEditData({ alias: acc.alias, provider_id: acc.provider_id, api_key: '', base_url: acc.base_url || '', anthropic_base_url: acc.anthropic_base_url || '', notes: acc.notes || '' });
    setEditSupportsAnthropic(!!acc.anthropic_base_url);
    setEditOpenAICompat(!!acc.openai_compatible);
    setIsEditOpen(true);
  };

  const handleEditSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!editingAccount) return;
    setIsValidating(true);
    setValidationError(null);
    try {
      await updateAccount(editingAccount.id, { ...editData, openai_compatible: editOpenAICompat ? 1 : 0, skip_validation: editSkipValidation });
      setIsEditOpen(false);
      setEditingAccount(null);
    } catch (err: any) {
      setValidationError(err.message || "Update validation failed");
    } finally {
      setIsValidating(false);
    }
  };

  const getSyncScript = () => {
    return `(async()=>{const p="${formData.provider_id}";console.log("🚀 LLMux Syncing...");const t=localStorage.getItem("token")||document.cookie;fetch("http://localhost:25975/api/auth/sync",{method:"POST",body:JSON.stringify({provider:p,token:t})})})();`;
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
                onChange={e => setFormData({...formData, alias: e.target.value})}
                placeholder={t('accounts.aliasPlaceholder')}
              />
            </div>
            <div className="space-y-1.5">
              <label className="text-xs font-bold text-muted-foreground uppercase">{t('accounts.provider')}</label>
              <select
                value={formData.provider_id}
                disabled={isValidating}
                onChange={e => {
                  const pid = e.target.value;
                  let burl = '';
                  if (pid === 'openai') burl = '';
                  else if (pid === 'anthropic') burl = '';
                  else if (pid === 'gemini') burl = '';
                  setFormData({...formData, provider_id: pid, base_url: burl});
                }}
                className="w-full h-10 px-3 py-2 rounded-md border border-input bg-background text-sm ring-offset-background focus:outline-none focus:ring-2 focus:ring-ring focus:ring-offset-2 disabled:opacity-50"
              >
                <option value="custom">{t('accounts.custom')}</option>
                <option value="custom-anthropic" hidden>{t('accounts.customAnthropic')}</option>
                <option value="openai">{t('accounts.openai')}</option>
                <option value="anthropic">{t('accounts.anthropic')}</option>
                <option value="gemini">{t('accounts.gemini')}</option>
              </select>
              <p className="text-xs text-muted-foreground mt-1">{t('accounts.providerHint')}</p>
            </div>
            <div className="space-y-1.5">
              <label className="text-xs font-bold text-muted-foreground uppercase">{t('accounts.apiKey')}</label>
              <Input
                type="password" required value={formData.api_key}
                disabled={isValidating}
                onChange={e => setFormData({...formData, api_key: e.target.value})}
                placeholder="sk-..."
                className="font-mono"
              />
            </div>
            <div className="space-y-1.5">
              <label className="text-xs font-bold text-muted-foreground uppercase">{t('accounts.baseUrl')}</label>
              <Input
                type="text" value={formData.base_url}
                disabled={isValidating}
                onChange={e => setFormData({...formData, base_url: e.target.value})}
                placeholder={formData.provider_id === 'anthropic' ? 'https://api.anthropic.com/v1' : formData.provider_id === 'gemini' ? 'https://generativelanguage.googleapis.com/v1beta' : 'https://api.openai.com/v1'}
                className="font-mono"
              />
              <p className="text-xs text-muted-foreground">{t('accounts.baseUrlHint')}</p>
            </div>
            {formData.provider_id === 'gemini' && (
              <div className="space-y-1.5 border-t border-border pt-3">
                <label className="flex items-center gap-2 cursor-pointer select-none">
                  <input
                    type="checkbox"
                    checked={formOpenAICompat}
                    disabled={isValidating}
                    onChange={e => setFormOpenAICompat(e.target.checked)}
                    className="w-4 h-4 rounded accent-primary"
                  />
                  <span className="text-xs font-bold text-muted-foreground uppercase">OpenAI 兼容模式</span>
                </label>
                <p className="text-xs text-muted-foreground ml-6">通过 Gemini 的 OpenAI 兼容端点同时支持 /v1/chat/completions 请求</p>
              </div>
            )}
            {formData.provider_id === 'custom' && (
              <>
                <div className="space-y-1.5 border-t border-border pt-3">
                  <label className="flex items-center gap-2 cursor-pointer select-none">
                    <input
                      type="checkbox"
                      checked={formSupportsAnthropic}
                      disabled={isValidating}
                      onChange={e => {
                        setFormSupportsAnthropic(e.target.checked);
                        if (!e.target.checked) setFormData({...formData, anthropic_base_url: ''});
                      }}
                      className="w-4 h-4 rounded accent-primary"
                    />
                    <span className="text-xs font-bold text-muted-foreground uppercase">{t('accounts.supportsAnthropic')}</span>
                  </label>
                  <p className="text-xs text-muted-foreground ml-6">{t('accounts.supportsAnthropicHint')}</p>
                </div>
                {formSupportsAnthropic && (
                  <div className="space-y-1.5 animate-in slide-in-from-top-1">
                    <label className="text-xs font-bold text-muted-foreground uppercase">{t('accounts.anthropicBaseUrl')}</label>
                    <Input
                      type="text" value={formData.anthropic_base_url}
                      disabled={isValidating}
                      onChange={e => setFormData({...formData, anthropic_base_url: e.target.value})}
                      placeholder={t('accounts.anthropicBaseUrlPlaceholder')}
                      className="font-mono"
                    />
                  </div>
                )}
              </>
            )}
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
                 disabled={isValidating}
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
                onChange={e => setEditData({...editData, alias: e.target.value})}
              />
            </div>
            <div className="space-y-1.5">
              <label className="text-xs font-bold text-muted-foreground uppercase">{t('accounts.provider')}</label>
              <select
                value={editData.provider_id}
                disabled={isValidating}
                onChange={e => setEditData({...editData, provider_id: e.target.value})}
                className="w-full h-10 px-3 py-2 rounded-md border border-input bg-background text-sm ring-offset-background focus:outline-none focus:ring-2 focus:ring-ring focus:ring-offset-2 disabled:opacity-50"
              >
                <option value="custom">{t('accounts.custom')}</option>
                <option value="custom-anthropic" hidden>{t('accounts.customAnthropic')}</option>
                <option value="openai">{t('accounts.openai')}</option>
                <option value="anthropic">{t('accounts.anthropic')}</option>
                <option value="gemini">{t('accounts.gemini')}</option>
              </select>
              <p className="text-xs text-muted-foreground mt-1">{t('accounts.providerHint')}</p>
            </div>
            <div className="space-y-1.5">
              <div className="flex items-center justify-between">
                <label className="text-xs font-bold text-muted-foreground uppercase">API Key</label>
                <span className="text-xs text-muted-foreground italic">{t('accounts.leaveBlank')}</span>
              </div>
              <Input
                type="password" value={editData.api_key}
                disabled={isValidating}
                onChange={e => setEditData({...editData, api_key: e.target.value})}
                placeholder={t('accounts.leaveBlank')}
                className="font-mono"
              />
            </div>
            <div className="space-y-1.5">
              <label className="text-xs font-bold text-muted-foreground uppercase">{t('accounts.baseUrl')}</label>
              <Input
                type="text" value={editData.base_url}
                disabled={isValidating}
                onChange={e => setEditData({...editData, base_url: e.target.value})}
                placeholder={editData.provider_id === 'anthropic' ? 'https://api.anthropic.com/v1' : editData.provider_id === 'gemini' ? 'https://generativelanguage.googleapis.com/v1beta' : 'https://api.openai.com/v1'}
                className="font-mono"
              />
              <p className="text-xs text-muted-foreground">{t('accounts.baseUrlHint')}</p>
            </div>
            {editData.provider_id === 'gemini' && (
              <div className="space-y-1.5 border-t border-border pt-3">
                <label className="flex items-center gap-2 cursor-pointer select-none">
                  <input
                    type="checkbox"
                    checked={editOpenAICompat}
                    disabled={isValidating}
                    onChange={e => setEditOpenAICompat(e.target.checked)}
                    className="w-4 h-4 rounded accent-primary"
                  />
                  <span className="text-xs font-bold text-muted-foreground uppercase">OpenAI 兼容模式</span>
                </label>
                <p className="text-xs text-muted-foreground ml-6">通过 Gemini 的 OpenAI 兼容端点同时支持 /v1/chat/completions 请求</p>
              </div>
            )}
            {editData.provider_id === 'custom' && (
              <>
                <div className="space-y-1.5 border-t border-border pt-3">
                  <label className="flex items-center gap-2 cursor-pointer select-none">
                    <input
                      type="checkbox"
                      checked={editSupportsAnthropic}
                      disabled={isValidating}
                      onChange={e => {
                        setEditSupportsAnthropic(e.target.checked);
                        if (!e.target.checked) setEditData({...editData, anthropic_base_url: ''});
                      }}
                      className="w-4 h-4 rounded accent-primary"
                    />
                    <span className="text-xs font-bold text-muted-foreground uppercase">{t('accounts.supportsAnthropic')}</span>
                  </label>
                  <p className="text-xs text-muted-foreground ml-6">{t('accounts.supportsAnthropicHint')}</p>
                </div>
                {editSupportsAnthropic && (
                  <div className="space-y-1.5 animate-in slide-in-from-top-1">
                    <label className="text-xs font-bold text-muted-foreground uppercase">{t('accounts.anthropicBaseUrl')}</label>
                    <Input
                      type="text" value={editData.anthropic_base_url}
                      disabled={isValidating}
                      onChange={e => setEditData({...editData, anthropic_base_url: e.target.value})}
                      placeholder={t('accounts.anthropicBaseUrlPlaceholder')}
                      className="font-mono"
                    />
                  </div>
                )}
              </>
            )}
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
                 disabled={isValidating}
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
