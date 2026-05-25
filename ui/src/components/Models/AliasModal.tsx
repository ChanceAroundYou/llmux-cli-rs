import React from 'react';
import { Save } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { Dialog } from '../Modal';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';

interface Props {
  isOpen: boolean;
  isEditing: boolean;
  alias: string;
  target: string;
  provider: string;
  selectedAccountIds: number[];
  safeModels: { id: string; owned_by: string }[];
  safeAccounts: { id: number; alias: string; provider_id: string; is_active: number }[];
  onAliasChange: (alias: string) => void;
  onTargetChange: (target: string) => void;
  onAccountIdsChange: (ids: number[]) => void;
  onClose: () => void;
  onSubmit: (e: React.FormEvent) => void;
}

export function AliasModal({
  isOpen, isEditing, alias, target, provider, selectedAccountIds,
  safeModels, safeAccounts,
  onAliasChange, onTargetChange, onAccountIdsChange, onClose, onSubmit,
}: Props) {
  const { t } = useTranslation();

  const matchingAliases = target
    ? [...new Set(safeModels.filter(x => x.id === target).map(x => x.owned_by))]
    : [];
  const matchingAccounts = safeAccounts.filter(a => matchingAliases.includes(a.alias) && a.is_active === 1);
  const otherAccounts = safeAccounts.filter(a => !matchingAliases.includes(a.alias) && a.is_active === 1);

  return (
    <Dialog isOpen={isOpen} onClose={onClose} title={isEditing ? t('models.editAlias') : t('models.createAlias')}>
      <form onSubmit={onSubmit} className="space-y-4">
        <div className="space-y-1.5">
          <label className="text-xs font-bold text-muted-foreground uppercase">{t('models.aliasName')}</label>
          <Input
            type="text" required value={alias}
            onChange={e => onAliasChange(e.target.value)}
            placeholder={t('models.aliasPlaceholder')}
          />
        </div>
        <div className="space-y-1.5">
          <label className="text-xs font-bold text-muted-foreground uppercase">{t('models.targetModel')}</label>
          <select
            value={target}
            onChange={e => onTargetChange(e.target.value)}
            className="w-full h-10 px-3 py-2 rounded-md border border-input bg-background text-sm ring-offset-background focus:outline-none focus:ring-2 focus:ring-ring focus:ring-offset-2"
          >
            <option value="">{t('common.default')}</option>
            {safeModels.map(mod => (
              <option key={`${mod.owned_by}:${mod.id}`} value={mod.id}>[{mod.owned_by}] {mod.id}</option>
            ))}
          </select>
        </div>
        {target && (
          <div className="space-y-1.5 border-t border-border pt-3">
            <label className="text-xs font-bold text-muted-foreground uppercase">
              {t('models.bindAccounts')}
              {matchingAccounts.length > 0 && (
                <span className="ml-1 text-primary font-normal">({matchingAccounts.length})</span>
              )}
            </label>
            <p className="text-xs text-muted-foreground">{t('models.bindAccountsHint')}</p>
            {matchingAccounts.length === 0 && (
              <p className="text-xs text-warning">{t('models.noAccountsForModel')}</p>
            )}
            {otherAccounts.length > 0 && (
              <p className="text-xs text-muted-foreground/60">{t('models.otherAccountsHidden', { count: otherAccounts.length })}</p>
            )}
            {matchingAccounts.length > 0 && (
              <div className="flex items-center gap-2">
                <button
                  type="button"
                  onClick={() => onAccountIdsChange(matchingAccounts.map(a => a.id))}
                  className="text-xs font-bold text-primary hover:underline"
                >{t('models.selectAll')}</button>
                <button
                  type="button"
                  onClick={() => onAccountIdsChange([])}
                  className="text-xs font-bold text-muted-foreground hover:underline"
                >{t('models.deselectAll')}</button>
              </div>
            )}
            <div className="max-h-32 overflow-y-auto space-y-1 border border-border rounded-lg p-2 bg-muted/30">
              {matchingAccounts.map(a => (
                <label key={a.id} className="flex items-center gap-2 px-2 py-1 hover:bg-muted/50 rounded cursor-pointer">
                  <input
                    type="checkbox"
                    checked={selectedAccountIds.includes(a.id)}
                    onChange={e => {
                      if (e.target.checked) {
                        onAccountIdsChange([...selectedAccountIds, a.id]);
                      } else {
                        onAccountIdsChange(selectedAccountIds.filter(id => id !== a.id));
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
          </div>
        )}
        <div className="pt-4 flex gap-3">
          <Button type="button" variant="outline" onClick={onClose} className="flex-1">{t('common.cancel')}</Button>
          <Button type="submit" className="flex-1">
            <Save size={16} /> {isEditing ? t('common.update') : t('common.save')}
          </Button>
        </div>
      </form>
    </Dialog>
  );
}
