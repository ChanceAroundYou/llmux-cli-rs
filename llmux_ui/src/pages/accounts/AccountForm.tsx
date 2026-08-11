// 账户表单弹窗：创建/编辑，含 URL 覆盖、权重、备注、跳过校验
import React, { useState, useEffect } from "react";
import { accountApi } from "@/api/accounts";
import type { AccountPublic, AccountCreatePayload } from "@/types/account";
import type { Vendor } from "@/types/vendor";
import { Modal } from "@/components/ui/Modal";
import { Button } from "@/components/ui/Button";
import { Input } from "@/components/ui/Input";
import { PasswordInput } from "@/components/ui/PasswordInput";
import { Select } from "@/components/ui/Select";
import { Switch } from "@/components/ui/Switch";
import { useToast } from "@/hooks/useToast";
import { useT } from "@/i18n";

interface Props {
  open: boolean;
  account: AccountPublic | null;
  vendors: Vendor[];
  onClose: () => void;
  onSuccess: () => void;
}

export const AccountFormModal: React.FC<Props> = ({
  open,
  account,
  vendors,
  onClose,
  onSuccess,
}) => {
  const { t } = useT();
  const isEdit = !!account;
  const toast = useToast();
  const [loading, setLoading] = useState(false);
  const [form, setForm] = useState({
    vendor_id: "",
    name: "",
    api_key: "",
    base_url: "",
    anthropic_base_url: "",
    enabled: true,
    weight: 1,
    openai_compatible: false,
    notes: "",
    skip_validation: false,
  });
  // 按所选厂商控制字段可用性：内置按支持的协议开关，自定义/未选全部可用
  const [urlFields, setUrlFields] = useState({
    baseEnabled: true,
    anthEnabled: true,
    compatEnabled: true,
  });

  useEffect(() => {
    if (account) {
      setForm({
        vendor_id: account.vendor_id,
        name: account.name,
        api_key: "********",
        base_url: account.base_url ?? "",
        anthropic_base_url: account.anthropic_base_url ?? "",
        enabled: !!account.enabled,
        weight: account.weight,
        openai_compatible: !!account.openai_compatible,
        notes: account.notes ?? "",
        skip_validation: false,
      });
    } else {
      setForm({
        vendor_id: "",
        name: "",
        api_key: "",
        base_url: "",
        anthropic_base_url: "",
        enabled: true,
        weight: 1,
        openai_compatible: false,
        notes: "",
        skip_validation: false,
      });
    }
  }, [account, open]);

  // 选择厂商：内置按支持的协议开关字段 + 自动填默认 URL；自定义/未选全部打开
  useEffect(() => {
    const v = vendors.find((x) => x.id === form.vendor_id);
    const isBuiltin = !!v && v.builtin === 1;
    const openai = v
      ? v.protocols.includes("openai") || v.protocol === "openai"
      : true;
    const anthropic = v ? v.protocols.includes("anthropic") : true;
    // OpenAI 协议端点可用性：支持 openai 协议，或 gemini（走 OpenAI 兼容端点）
    const base = v ? openai || v.protocol === "gemini" : true;

    setUrlFields({
      baseEnabled: v ? (isBuiltin ? base : true) : true,
      anthEnabled: v ? (isBuiltin ? anthropic : true) : true,
      compatEnabled: v ? (isBuiltin ? base : true) : true,
    });

    // 自动填默认 URL（仅当字段为空，避免覆盖编辑时已保存的自定义 URL）
    if (v) {
      setForm((f) => ({
        ...f,
        base_url: f.base_url || (v.default_base_url ?? ""),
        anthropic_base_url:
          f.anthropic_base_url || (v.default_anthropic_url ?? ""),
      }));
    }
  }, [form.vendor_id, vendors]);

  const handleSubmit = async () => {
    setLoading(true);
    try {
      if (isEdit && account?.id) {
        await accountApi.update(account.id, {
          ...form,
          enabled: form.enabled ? 1 : 0,
          openai_compatible: form.openai_compatible ? 1 : 0,
        });
        toast.success(t("accounts.updated"));
      } else {
        const res = await accountApi.create({
          ...form,
          enabled: form.enabled ? 1 : 0,
          openai_compatible: form.openai_compatible ? 1 : 0,
        } as AccountCreatePayload);
        toast.success(t("accounts.created", { n: res.modelCount }));
      }
      onSuccess();
    } catch (err) {
      toast.error(err instanceof Error ? err.message : t("accounts.operationFailed"));
    } finally {
      setLoading(false);
    }
  };

  const label = "mb-1 block text-sm font-medium text-card-foreground";

  return (
    <Modal
      open={open}
      onClose={onClose}
      title={isEdit ? t("accounts.form.edit") : t("accounts.form.add")}
      size="lg"
      footer={
        <>
          <Button variant="secondary" onClick={onClose}>
            {t("common.cancel")}
          </Button>
          <Button loading={loading} onClick={handleSubmit}>
            {isEdit ? t("accounts.form.saveChanges") : t("accounts.form.createAccount")}
          </Button>
        </>
      }
    >
      <div className="space-y-4">
        <div className="grid grid-cols-2 gap-4">
          <div>
            <label className={label}>{t("accounts.form.vendor")} *</label>
            <Select
              value={form.vendor_id}
              onChange={(v) => setForm((f) => ({ ...f, vendor_id: v }))}
              options={vendors.map((v) => ({ value: v.id, label: v.name }))}
              placeholder={t("accounts.form.selectVendor")}
            />
          </div>
          <div>
            <label className={label}>{t("accounts.form.name")} *</label>
            <Input
              value={form.name}
              onChange={(v) => setForm((f) => ({ ...f, name: v }))}
              placeholder="My Account"
            />
          </div>
        </div>

        <div>
          <label className={label}>
            {t("accounts.form.apiKey")} *{" "}
            {isEdit && (
              <span className="text-xs text-muted-foreground">{t("accounts.form.apiKeyKeep")}</span>
            )}
          </label>
          <PasswordInput
            value={form.api_key}
            onChange={(v) => setForm((f) => ({ ...f, api_key: v }))}
            placeholder="sk-..."
          />
        </div>

        <div className="grid grid-cols-2 gap-4">
          <div>
            <label className={label}>{t("accounts.form.baseUrl")}</label>
            <Input
              value={form.base_url}
              onChange={(v) => setForm((f) => ({ ...f, base_url: v }))}
              placeholder="https://api.openai.com/v1"
              disabled={!urlFields.baseEnabled}
            />
          </div>
          <div>
            <label className={label}>{t("accounts.form.anthropicUrl")}</label>
            <Input
              value={form.anthropic_base_url}
              onChange={(v) => setForm((f) => ({ ...f, anthropic_base_url: v }))}
              placeholder="https://api.anthropic.com"
              disabled={!urlFields.anthEnabled}
            />
          </div>
        </div>

        <div className="grid grid-cols-3 gap-4">
          <div>
            <label className={label}>{t("accounts.form.weight")}</label>
            <Input
              type="number"
              value={String(form.weight)}
              onChange={(v) => setForm((f) => ({ ...f, weight: Number(v) || 1 }))}
            />
          </div>
          <div className="flex flex-col gap-3">
            <Switch
              label={t("accounts.form.enabled")}
              checked={form.enabled}
              onChange={(v) => setForm((f) => ({ ...f, enabled: v }))}
            />
            <Switch
              label={t("accounts.form.openaiCompat")}
              checked={form.openai_compatible}
              disabled={!urlFields.compatEnabled}
              onChange={(v) => setForm((f) => ({ ...f, openai_compatible: v }))}
            />
          </div>
        </div>

        <div>
          <label className={label}>{t("accounts.form.notes")}</label>
          <Input
            value={form.notes}
            onChange={(v) => setForm((f) => ({ ...f, notes: v }))}
            placeholder={t("accounts.form.notesPlaceholder")}
          />
        </div>

        <Switch
          label={t("accounts.form.skipValidation")}
          description={t("accounts.form.skipValidationDesc")}
          checked={form.skip_validation}
          onChange={(v) => setForm((f) => ({ ...f, skip_validation: v }))}
        />
      </div>
    </Modal>
  );
};
