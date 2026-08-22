import React, { useState, useEffect } from 'react';
import { Save, Zap, Loader2, CheckCircle2, XCircle } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { Dialog } from '../Modal';
import { cn } from '@/lib/utils';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';

interface Account {
  id: number;
  alias: string;
  provider_id: string;
  is_active: number;
}

interface ModelItem {
  id: string;
  owned_by: string;
}

interface VerifyResult {
  success: boolean;
  error?: string;
  latency?: number;
}

interface Props {
  isOpen: boolean;
  accountId: string;
  alias: string;
  target: string;
  safeModels: ModelItem[];
  safeAccounts: Account[];
  onAccountIdChange: (id: string) => void;
  onAliasChange: (alias: string) => void;
  onTargetChange: (target: string) => void;
  onClose: () => void;
  onSubmit: (e: React.FormEvent) => void;
  testModel: (modelId: string, providerId: string, accountId?: number) => Promise<VerifyResult>;
}

export function CustomAliasModal({
  isOpen, accountId, alias, target, safeModels, safeAccounts,
  onAccountIdChange, onAliasChange, onTargetChange, onClose, onSubmit, testModel,
}: Props) {
  const { t } = useTranslation();
  const [isVerifying, setIsVerifying] = useState(false);
  const [verifyResult, setVerifyResult] = useState<VerifyResult | null>(null);

  useEffect(() => {
    if (isOpen) {
      setIsVerifying(false);
      setVerifyResult(null);
    }
  }, [isOpen]);

  const handleVerify = async () => {
    if (!target || !accountId) return;
    const account = safeAccounts.find(a => a.id === Number(accountId));
    if (!account) return;
    setIsVerifying(true);
    setVerifyResult(null);
    try {
      const result = await testModel(target, account.provider_id, account.id);
      setVerifyResult(result);
    } catch (err: any) {
      setVerifyResult({ success: false, error: err.message });
    } finally {
      setIsVerifying(false);
    }
  };

  return (
    <Dialog isOpen={isOpen} onClose={onClose} title={t('models.customAliasTitle')}>
      <form onSubmit={onSubmit} className="space-y-4">
        <div className="space-y-1.5">
          <label className="text-xs font-bold text-muted-foreground uppercase">{t('models.selectAccount')}</label>
          <select
            value={accountId}
            onChange={e => {
              onAccountIdChange(e.target.value);
              onTargetChange('');
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
            type="text" required value={alias}
            onChange={e => onAliasChange(e.target.value)}
            placeholder={t('models.aliasPlaceholder')}
          />
        </div>
        <div className="space-y-1.5">
          <label className="text-xs font-bold text-muted-foreground uppercase">{t('models.manualModel')}</label>
          <Input
            type="text" required value={target}
            onChange={e => {
              onTargetChange(e.target.value);
              setVerifyResult(null);
            }}
            placeholder={t('models.manualModelPlaceholder')}
            list="custom-model-options"
          />
          <datalist id="custom-model-options">
            {safeModels
              .filter(m => {
                if (!accountId) return true;
                const account = safeAccounts.find(a => a.id === Number(accountId));
                return account ? m.owned_by === account.provider_id : true;
              })
              .map(m => (
                <option key={m.id} value={m.id} />
              ))}
          </datalist>
        </div>

        <div className="space-y-2">
          <Button
            type="button"
            variant="outline"
            onClick={handleVerify}
            disabled={isVerifying || !target || !accountId}
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
          <Button type="button" variant="outline" onClick={onClose} className="flex-1">{t('common.cancel')}</Button>
          <Button type="submit" disabled={!verifyResult?.success} className="flex-1">
            <Save size={16} /> {t('common.save')}
          </Button>
        </div>
      </form>
    </Dialog>
  );
}
