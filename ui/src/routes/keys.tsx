import React, { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { useKeysStore, ApiKey } from '../stores/keys';
import { useModelsStore } from '../stores/models';
import { 
  Key, 
  Plus, 
  Copy, 
  Trash2, 
  ShieldCheck, 
  Check,
  Calendar,
  Lock,
  Eye,
  EyeOff,
  Terminal,
  BookOpen,
  Edit2
} from 'lucide-react';
import { Dialog, ConfirmDialog } from '../components/Modal';
import { CopyButton } from '../components/CopyButton';
import { parseServerDate } from '../utils/date';

function cn(...classes: (string | undefined | null | false)[]) {
  return classes.filter(Boolean).join(' ');
}

function parseAllowedModels(raw: string): string[] {
  if (!raw || raw === '*') return [];
  try {
    const parsed = JSON.parse(raw);
    return Array.isArray(parsed) ? parsed : [];
  } catch {
    return [];
  }
}

export default function KeysPage() {
  const { t } = useTranslation();
  const { keys, isLoading, fetchKeys, createKey, deleteKey, updateKey } = useKeysStore();
  const { availableModels, aliases, fetchModels, fetchAliases } = useModelsStore();
  
  const [isModalOpen, setIsModalOpen] = useState(false);
  const [editingKey, setEditingKey] = useState<ApiKey | null>(null);
  const [keyToDelete, setKeyToDelete] = useState<ApiKey | null>(null);
  const [newKeyData, setNewKeyData] = useState({ name: '', allowedModels: '*' as '*' | string[] });
  const [generatedKey, setGeneratedKey] = useState<string | null>(null);
  const [visibleKeys, setVisibleKeys] = useState<Record<number, boolean>>({});
  const [showWarningModal, setShowWarningModal] = useState(false);
  const [warningKeyNames, setWarningKeyNames] = useState<string[]>([]);

  const baseUrl = typeof window !== 'undefined' ? `${window.location.protocol}//${window.location.host}/v1` : 'http://localhost:25975/v1';

  useEffect(() => {
    fetchKeys();
    fetchModels();
    fetchAliases();
  }, []);

  useEffect(() => {
    if (!isLoading && keys.length > 0) {
      // 检查是否有由于别名删除导致的“空授权”Key (且不是 '*' 全部授权)
      const emptyKeys = keys.filter(k => k.allowed_models !== '*' && parseAllowedModels(k.allowed_models).length === 0);
      if (emptyKeys.length > 0) {
        setWarningKeyNames(emptyKeys.map(k => k.name));
        setShowWarningModal(true);
      }
    }
  }, [isLoading, keys]);

  const handleCreateOrUpdate = async (e: React.FormEvent) => {
    e.preventDefault();
    try {
      if (editingKey) {
        await updateKey(editingKey.id, newKeyData.name, newKeyData.allowedModels);
        setIsModalOpen(false);
      } else {
        const key = await createKey(newKeyData.name, newKeyData.allowedModels);
        setGeneratedKey(key);
      }
    } catch (err) {
      console.error("Operation error:", err);
    }
  };

  const copyToClipboard = (text: string) => {
    navigator.clipboard.writeText(text);
  };

  const toggleVisibility = (id: number) => {
    setVisibleKeys(prev => ({ ...prev, [id]: !prev[id] }));
  };

  // 限制：只能选择已创建别名的模型
  const sortedModels = aliases.map(a => ({ id: a.alias, provider: a.provider_id }));

  // 提交条件：有别名才能创建 key；指定模型时必须至少勾选一个
  const canSubmit = sortedModels.length > 0 && (
    newKeyData.allowedModels === '*' ||
    (Array.isArray(newKeyData.allowedModels) && newKeyData.allowedModels.length > 0)
  );

  return (
    <div className="space-y-8 animate-in fade-in duration-500">
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-3">
          <div className="p-2 bg-primary/10 text-primary rounded-lg">
            <Key size={24} />
          </div>
          <div>
            <h1 className="text-2xl font-bold tracking-tight">{t('keys.title')}</h1>
            <p className="text-sm text-muted-foreground">{t('keys.subtitle')}</p>
          </div>
        </div>
        <button
          onClick={() => {
            setGeneratedKey(null);
            setEditingKey(null);
            setNewKeyData({ name: '', allowedModels: '*' });
            setIsModalOpen(true);
          }}
          className="flex items-center gap-2 px-4 py-2 bg-primary text-primary-foreground rounded-lg text-sm font-medium hover:opacity-90 transition-all shadow-sm"
        >
          <Plus size={16} />
          {t('keys.createKey')}
        </button>
      </div>

      <div className="p-5 bg-card border border-border rounded-2xl flex flex-col md:flex-row md:items-center justify-between gap-4 relative overflow-hidden">
        <div className="absolute inset-0 bg-gradient-to-r from-primary/5 to-transparent pointer-events-none" />
        <div className="space-y-1.5 relative">
          <div className="font-bold flex items-center gap-2 text-sm">
            <Terminal size={16} className="text-primary" />
            {t('keys.apiUsage')}
          </div>
          <div className="text-xs text-muted-foreground font-medium max-w-xl leading-relaxed">
            {t('keys.apiDesc')}
          </div>
        </div>
        <div className="flex items-center gap-2 bg-muted/50 p-2 rounded-xl border border-border/50 relative">
          <span className="text-[10px] font-black uppercase text-muted-foreground/60 border-r border-border/50 pr-2 mr-1 ml-1">{t('keys.endpoint')}</span>
          <code className="text-sm font-mono font-bold select-all text-primary">{baseUrl}</code>
          <CopyButton value={baseUrl} size={14} title={t('keys.copyBase')} />
        </div>
      </div>

      <div className="grid gap-4">
        {keys.map((k) => (
          <div key={k.id} className="p-5 rounded-2xl border border-border bg-card hover:border-primary/30 transition-all group relative overflow-hidden">
            <div className="flex flex-col md:flex-row md:items-center justify-between gap-4">
              <div className="space-y-1 flex-1">
                <div className="flex items-center gap-2">
                  <h3 className="font-bold text-base">{k.name}</h3>
                  <span className="text-[10px] bg-muted px-2 py-0.5 rounded-full font-bold text-muted-foreground uppercase tracking-tight flex items-center gap-1">
                    <Calendar size={10} />
                    {parseServerDate(k.created_at).toLocaleString(undefined, {
                      year: 'numeric', month: 'short', day: 'numeric'
                    })}
                  </span>
                </div>
                <div className="flex items-center gap-2 font-mono text-sm bg-muted/30 p-2 rounded-lg border border-border/50 group/key max-w-2xl">
                  <span className="text-muted-foreground truncate">
                    {visibleKeys[k.id] ? k.key : '••••••••••••••••••••••••'}
                  </span>
                  <div className="flex items-center gap-1 ml-auto shrink-0">
                    <button 
                      onClick={() => toggleVisibility(k.id)}
                      className="p-1 hover:bg-background rounded transition-colors text-muted-foreground hover:text-foreground"
                    >
                      {visibleKeys[k.id] ? <EyeOff size={14} /> : <Eye size={14} />}
                    </button>
                    <CopyButton value={k.key} size={14} />
                  </div>
                </div>
                {k.allowed_models !== '*' && (
                   <div className="flex flex-wrap gap-1.5 mt-2">
                      {parseAllowedModels(k.allowed_models).map((m: string) => (
                         <div key={m} className="flex items-center gap-1.5 pl-2.5 pr-1 py-0.5 bg-primary/5 text-primary/80 text-[11px] font-black rounded border border-primary/10 group/tag hover:bg-primary/10 transition-colors shadow-sm">
                            {m}
                            <CopyButton value={m} size={9} className="p-0.5 opacity-0 group-hover/tag:opacity-100 transition-opacity" />
                         </div>
                      ))}
                   </div>
                )}
              </div>

              <div className="flex items-center gap-3">
                <div className="text-right mr-3 hidden sm:block">
                  <div className="text-[10px] font-bold text-muted-foreground uppercase tracking-widest mb-1">{t('keys.permissions')}</div>
                  <div className="flex items-center gap-1.5 justify-end">
                    <ShieldCheck size={14} className={k.allowed_models === '*' ? 'text-green-500' : 'text-blue-500'} />
                    <span className="text-xs font-bold capitalize">
                      {k.allowed_models === '*' ? t('keys.allModels') : t('keys.specificModels', { count: parseAllowedModels(k.allowed_models).length })}
                    </span>
                  </div>
                </div>
                <div className="flex items-center gap-1">
                  <button
                    onClick={() => {
                      setEditingKey(k);
                      setGeneratedKey(null);
                      setNewKeyData({ 
                        name: k.name, 
                        allowedModels: k.allowed_models === '*' ? '*' : parseAllowedModels(k.allowed_models) 
                      });
                      setIsModalOpen(true);
                    }}
                    className="p-2.5 hover:bg-primary/10 hover:text-primary rounded-xl text-muted-foreground transition-all"
                    title={t('common.edit')}
                  >
                    <Edit2 size={18} />
                  </button>
                  <button
                    onClick={() => setKeyToDelete(k)}
                    className="p-2.5 hover:bg-red-500/10 hover:text-red-500 rounded-xl text-muted-foreground transition-all"
                    title={t('common.delete')}
                  >
                    <Trash2 size={18} />
                  </button>
                </div>
              </div>
            </div>
          </div>
        ))}

        {!isLoading && keys.length === 0 && (
          <div className="py-24 text-center border-2 border-dashed border-border rounded-3xl">
             <div className="w-12 h-12 bg-muted rounded-full flex items-center justify-center mx-auto mb-4 text-muted-foreground/30">
                <Lock size={24} />
             </div>
             <p className="text-sm text-muted-foreground font-medium">{t('keys.noKeysYet')}</p>
          </div>
        )}
      </div>

      <Dialog 
        isOpen={isModalOpen} 
        onClose={() => setIsModalOpen(false)} 
        title={generatedKey ? t('keys.keyCreated') : (editingKey ? t('common.edit') : t('keys.createKey'))}
      >
        {generatedKey ? (
          <div className="space-y-6 animate-in zoom-in-95 duration-300">
            <div className="p-4 bg-primary/5 border border-primary/10 rounded-2xl space-y-3">
              <div className="flex items-center justify-between">
                <span className="text-xs font-bold text-primary uppercase tracking-widest">{t('keys.secretKey')}</span>
                <span className="text-[10px] text-amber-600 font-bold bg-amber-500/10 px-2 py-0.5 rounded">{t('keys.saveThisNow')}</span>
              </div>
              <div className="flex items-center gap-3 bg-card p-3 rounded-xl border border-border shadow-sm">
                <code className="text-sm font-mono flex-1 truncate">{generatedKey}</code>
                <CopyButton 
                  value={generatedKey} 
                  size={16} 
                  className="bg-primary hover:bg-primary text-primary-foreground" 
                />
              </div>
            </div>
            <button
              onClick={() => setIsModalOpen(false)}
              className="w-full py-3 bg-muted hover:bg-muted/80 rounded-xl font-bold transition-all"
            >
              {t('common.done')}
            </button>
          </div>
        ) : (
          <form onSubmit={handleCreateOrUpdate} className="space-y-6">
            <div className="space-y-2">
              <label className="text-xs font-bold text-muted-foreground uppercase px-1">{t('keys.keyName')}</label>
              <input 
                type="text" 
                required
                value={newKeyData.name}
                onChange={e => setNewKeyData({...newKeyData, name: e.target.value})}
                placeholder={t('keys.keyPlaceholder')}
                className="w-full px-4 py-3 bg-muted/50 border border-border rounded-xl outline-none focus:ring-2 focus:ring-primary/20 transition-all"
              />
            </div>

            <div className="space-y-3">
              <label className="text-xs font-bold text-muted-foreground uppercase px-1">{t('keys.permissions')}</label>
              <div className="grid grid-cols-2 gap-2">
                <button
                  type="button"
                  onClick={() => setNewKeyData({...newKeyData, allowedModels: '*'})}
                  className={cn(
                    "p-3 rounded-xl border text-sm font-bold transition-all flex items-center justify-center gap-2",
                    newKeyData.allowedModels === '*' 
                      ? "bg-primary/10 border-primary text-primary" 
                      : "bg-card border-border text-muted-foreground hover:border-primary/30"
                  )}
                >
                  <ShieldCheck size={16} />
                  {t('keys.allModels')}
                </button>
                <button
                  type="button"
                  onClick={() => setNewKeyData({...newKeyData, allowedModels: Array.isArray(newKeyData.allowedModels) ? newKeyData.allowedModels : []})}
                  className={cn(
                    "p-3 rounded-xl border text-sm font-bold transition-all flex items-center justify-center gap-2",
                    newKeyData.allowedModels !== '*' 
                      ? "bg-primary/10 border-primary text-primary" 
                      : "bg-card border-border text-muted-foreground hover:border-primary/30"
                  )}
                >
                  <Plus size={16} />
                  {t('keys.selectedModels')}
                </button>
              </div>
            </div>

            {newKeyData.allowedModels !== '*' && (
              <div className="max-h-48 overflow-y-auto p-2 border border-border rounded-xl space-y-1 bg-muted/20">
                {sortedModels.length > 0 ? (
                  sortedModels.map(item => (
                    <label key={item.id} className="flex items-center gap-3 p-2 hover:bg-muted rounded-lg cursor-pointer transition-colors group">
                      <input 
                        type="checkbox"
                        checked={Array.isArray(newKeyData.allowedModels) && newKeyData.allowedModels.includes(item.id)}
                        onChange={(e) => {
                          const current = Array.isArray(newKeyData.allowedModels) ? newKeyData.allowedModels : [];
                          if (e.target.checked) {
                            setNewKeyData({...newKeyData, allowedModels: [...current, item.id]});
                          } else {
                            setNewKeyData({...newKeyData, allowedModels: current.filter(x => x !== item.id)});
                          }
                        }}
                        className="w-4 h-4 rounded border-border text-primary focus:ring-primary/20"
                      />
                      <div className="flex flex-1 items-center justify-between min-w-0">
                        <span className="text-xs font-medium truncate">{item.id}</span>
                        <span className="shrink-0 text-[10px] font-black bg-primary/10 text-primary px-2 py-0.5 rounded uppercase tracking-tight shadow-sm border border-primary/5">{t('common.alias')}</span>
                      </div>
                    </label>
                  ))
                ) : (
                  <div className="py-8 text-center px-4">
                    <p className="text-xs text-muted-foreground italic leading-relaxed">
                      {t('keys.noAliasesHint')}
                    </p>
                  </div>
                )}
              </div>
            )}

            {!canSubmit && (
              <p className="text-center text-xs text-muted-foreground pb-1">
                {sortedModels.length === 0
                  ? t('keys.noAliasesHint')
                  : t('keys.selectModelHint', '请至少选择一个模型')}
              </p>
            )}
            <div className="flex gap-3 pt-2">
              <button 
                type="button" 
                onClick={() => setIsModalOpen(false)}
                className="flex-1 py-3 border border-border rounded-xl font-bold hover:bg-muted transition-all"
              >
                {t('common.cancel')}
              </button>
              <button
                type="submit"
                disabled={!canSubmit}
                className="flex-1 py-3 bg-primary text-primary-foreground rounded-xl font-bold hover:opacity-90 transition-all disabled:opacity-40 disabled:cursor-not-allowed"
              >
                {t('common.save')}
              </button>
            </div>
          </form>
        )}
      </Dialog>

      <ConfirmDialog
        isOpen={!!keyToDelete}
        onClose={() => setKeyToDelete(null)}
        onConfirm={async () => {
          if (keyToDelete) {
            await deleteKey(keyToDelete.id);
            setKeyToDelete(null);
          }
        }}
        title={t('keys.revokeKey')}
        description={t('keys.deleteConfirm', { name: keyToDelete?.name })}
        confirmText={t('keys.revoke')}
        variant="danger"
      />

      {/* 空权限异常警告 Modal */}
      <Dialog
        isOpen={showWarningModal}
        onClose={() => setShowWarningModal(false)}
        title={t('keys.emptyAuthWarning')}
      >
        <div className="space-y-4">
          <div className="p-4 bg-amber-500/10 border border-amber-500/20 rounded-2xl flex gap-3 text-amber-700">
             <ShieldCheck size={20} className="shrink-0" />
             <div className="space-y-2">
                <p className="text-sm font-bold">{t('keys.emptyAuthDesc')}</p>
                <div className="flex flex-wrap gap-2">
                   {warningKeyNames.map(name => (
                     <span key={name} className="px-2 py-1 bg-amber-500/20 rounded text-[10px] font-black uppercase tracking-tight">
                        {name}
                     </span>
                   ))}
                </div>
             </div>
          </div>
          <button 
            onClick={() => setShowWarningModal(false)}
            className="w-full py-3 bg-primary text-primary-foreground rounded-xl font-bold shadow-lg shadow-primary/20 transition-all hover:opacity-90"
          >
            {t('common.done')}
          </button>
        </div>
      </Dialog>
    </div>
  );
}
