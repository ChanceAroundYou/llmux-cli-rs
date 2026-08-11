// 新增/编辑别名：选目标模型 → 智能匹配厂商账户 → 分组勾选 + 首选
import React, { useState, useEffect, useCallback } from "react";
import type { AvailableModel, AliasResponse } from "@/types/model";
import type { AccountPublic } from "@/types/account";
import { Modal } from "@/components/ui/Modal";
import { Button } from "@/components/ui/Button";
import { Input } from "@/components/ui/Input";
import { Select } from "@/components/ui/Select";
import { useT } from "@/i18n";

export interface AliasPayload {
  alias: string;
  target_model: string;
  vendor_id?: string;
  account_ids?: number[];
  preferred_account_id?: number;
}

interface Props {
  open: boolean;
  editing: AliasResponse | null;
  models: AvailableModel[];
  accounts: AccountPublic[];
  saving: boolean;
  onClose: () => void;
  onSubmit: (payload: AliasPayload) => void;
  /** 预填的目标模型（快速关联场景） */
  initialTarget?: string;
}

const labelCls = "mb-1 block text-sm font-medium text-card-foreground";

export const AliasFormModal: React.FC<Props> = ({
  open,
  editing,
  models,
  accounts,
  saving,
  onClose,
  onSubmit,
  initialTarget,
}) => {
  const { t } = useT();
  const [alias, setAlias] = useState("");
  const [target, setTarget] = useState("");
  const [selectedIds, setSelectedIds] = useState<number[]>([]);
  const [preferredId, setPreferredId] = useState("");

  // 根据目标模型算出匹配的启用账户 id（智能预填用）
  const idsForTarget = useCallback(
    (tgt: string) =>
      tgt
        ? accounts
            .filter((a) => {
              const vendors = new Set(models.filter((m) => m.id === tgt).map((m) => m.owned_by));
              return vendors.has(a.vendor_id) && a.enabled;
            })
            .map((a) => a.id)
            .filter((id): id is number => id != null)
        : [],
    [models, accounts],
  );

  useEffect(() => {
    if (open) {
      const initTarget = editing ? editing.target_model : (initialTarget ?? "");
      setAlias(editing ? editing.alias : "");
      setTarget(initTarget);
      setSelectedIds(editing ? editing.accounts.map((a) => a.id) : idsForTarget(initTarget));
      setPreferredId(editing?.preferred_account_id ? String(editing.preferred_account_id) : "");
    }
  }, [open, editing, initialTarget, models, accounts, idsForTarget]);

  // 目标模型匹配的厂商 → 这些厂商下的启用账户
  const matchingVendors = target
    ? [...new Set(models.filter((m) => m.id === target).map((m) => m.owned_by))]
    : [];
  const matchingAccounts = accounts.filter((a) => matchingVendors.includes(a.vendor_id) && a.enabled);
  const grouped = matchingAccounts.reduce<Record<string, AccountPublic[]>>((acc, a) => {
    (acc[a.vendor_id] ||= []).push(a);
    return acc;
  }, {});

  const handleTargetChange = (v: string) => {
    setTarget(v);
    // 智能预填：选目标后自动勾选匹配的启用账户
    setSelectedIds(idsForTarget(v));
    setPreferredId("");
  };

  const toggleAccount = (id: number) => {
    setSelectedIds((prev) => (prev.includes(id) ? prev.filter((x) => x !== id) : [...prev, id]));
  };

  const selectedAccounts = accounts.filter((a) => a.id != null && selectedIds.includes(a.id!));

  const submit = () => {
    if (!alias.trim() || !target.trim()) return;
    onSubmit({
      alias: alias.trim(),
      target_model: target.trim(),
      // 目标模型所属厂商（别名可能聚合多模型，仍以目标模型为准）
      vendor_id: models.find((m) => m.id === target)?.owned_by,
      account_ids: selectedIds,
      preferred_account_id: preferredId ? Number(preferredId) : undefined,
    });
  };

  return (
    <Modal
      open={open}
      onClose={onClose}
      title={editing ? t("aliases.form.editTitle") : t("aliases.form.createTitle")}
      size="lg"
      footer={
        <>
          <Button variant="secondary" onClick={onClose}>
            {t("common.cancel")}
          </Button>
          <Button loading={saving} onClick={submit}>
            {editing ? t("common.save") : t("aliases.form.createTitle")}
          </Button>
        </>
      }
    >
      <div className="space-y-4">
        <div>
          <label className={labelCls}>{t("aliases.form.alias")} *</label>
          <Input value={alias} onChange={setAlias} placeholder="my-gpt4" />
        </div>

        <div>
          <label className={labelCls}>{t("aliases.form.selectTarget")} *</label>
          <Select
            value={target}
            onChange={handleTargetChange}
            options={models.map((m) => ({ value: m.id, label: `[${m.owned_by}] ${m.id}` }))}
            placeholder={t("aliases.form.selectTarget")}
          />
        </div>

        {target && (
          <div>
            <label className={labelCls}>
              {t("aliases.form.bindAccounts")} ({matchingAccounts.length})
            </label>
            <p className="mb-2 text-xs text-muted-foreground">{t("aliases.form.bindHint")}</p>

            {matchingAccounts.length === 0 ? (
              <p className="text-xs text-warning-foreground">{t("aliases.form.noMatchingAccounts")}</p>
            ) : (
              <>
                <div className="mb-1 flex items-center gap-3 text-xs font-semibold">
                  <button
                    type="button"
                    className="text-primary hover:underline"
                    onClick={() =>
                      setSelectedIds(matchingAccounts.map((a) => a.id).filter((id): id is number => id != null))
                    }
                  >
                    {t("aliases.form.selectAll")}
                  </button>
                  <button type="button" className="text-muted-foreground hover:underline" onClick={() => setSelectedIds([])}>
                    {t("aliases.form.deselectAll")}
                  </button>
                </div>
                <div className="max-h-40 space-y-1 overflow-y-auto rounded-xl border border-border bg-muted/40 p-2">
                  {Object.entries(grouped).map(([vid, accts]) => (
                    <div key={vid}>
                      <div className="px-2 py-1 text-[10px] font-semibold uppercase tracking-wide text-muted-foreground">
                        [{vid}] ({accts.length})
                      </div>
                      {accts.map((a) => (
                        <label key={a.id} className="flex cursor-pointer items-center gap-2 rounded-lg px-2 py-1 transition-colors hover:bg-muted/60">
                          <input
                            type="checkbox"
                            checked={a.id != null && selectedIds.includes(a.id!)}
                            onChange={() => a.id != null && toggleAccount(a.id!)}
                            className="h-3.5 w-3.5 rounded accent-primary"
                          />
                          <span className="text-xs text-card-foreground">{a.name}</span>
                        </label>
                      ))}
                    </div>
                  ))}
                </div>

                <div className="mt-3">
                  <label className={labelCls}>{t("aliases.form.preferredAccount")}</label>
                  <Select
                    value={preferredId}
                    onChange={setPreferredId}
                    options={selectedAccounts.map((a) => ({ value: String(a.id), label: `[${a.vendor_id}] ${a.name}` }))}
                    placeholder={t("aliases.form.preferredAccount")}
                  />
                  <p className="mt-1 text-xs text-muted-foreground">{t("aliases.form.preferredHint")}</p>
                </div>
              </>
            )}
          </div>
        )}
      </div>
    </Modal>
  );
};
