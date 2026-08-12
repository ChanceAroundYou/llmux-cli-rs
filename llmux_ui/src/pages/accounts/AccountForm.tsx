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
    // 编辑态暂存：厂商协议不支持某字段被清空前的原值，切回时恢复
    _prev_base_url: "",
    _prev_anthropic_url: "",
  });
  // 按所选厂商控制字段可用性：内置按支持的协议开关，自定义/未选全部可用
  const [urlFields, setUrlFields] = useState({
    baseEnabled: true,
    anthEnabled: true,
    compatEnabled: true,
    supportsCoding: false,
  });
  // 是否使用厂商的 Coding Plan 端点（厂商提供 coding URL 时才可选）
  const [useCodingPlan, setUseCodingPlan] = useState(false);

  useEffect(() => {
    if (account) {
      // 编辑态：账户 base_url 等于厂商 coding URL → 默认视为使用 coding plan
      const vendor = vendors.find((x) => x.id === account.vendor_id);
      const usesCoding =
        !!vendor &&
        ((account.base_url ?? "") === (vendor.coding_base_url ?? "") ||
          (account.base_url ?? "") === (vendor.coding_anthropic_url ?? ""));
      setUseCodingPlan(usesCoding);
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
        _prev_base_url: "",
        _prev_anthropic_url: "",
      });
    } else {
      setUseCodingPlan(false);
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
        _prev_base_url: "",
        _prev_anthropic_url: "",
      });
    }
  }, [account, open, vendors]);

  // 厂商协议 → URL 字段可用性（内置按协议开关，自定义全开）。
  // handleVendorChange / toggleCodingPlan / urlFields effect 共用，避免判断分叉。
  const vendorUrlState = (v: Vendor | undefined) => {
    if (!v)
      return {
        baseEnabled: true,
        anthEnabled: true,
        compatEnabled: true,
        supportsCoding: false,
      };
    const isBuiltin = v.builtin === 1;
    const openai = v.protocols.includes("openai") || v.protocol === "openai";
    const anthropic = v.protocols.includes("anthropic");
    // OpenAI 协议端点可用性：支持 openai 协议，或 gemini（走 OpenAI 兼容端点）
    const base = openai || v.protocol === "gemini";
    // Coding Plan：厂商提供 coding 端点（任一）即支持——无需开关，填了 URL 即生效
    const supportsCoding = !!v.coding_base_url || !!v.coding_anthropic_url;
    return {
      baseEnabled: isBuiltin ? base : true,
      anthEnabled: isBuiltin ? anthropic : true,
      compatEnabled: isBuiltin ? base : true,
      supportsCoding,
    };
  };

  // 只计算字段可用性（vendor 变化时同步），不在此填 URL
  useEffect(() => {
    const v = vendors.find((x) => x.id === form.vendor_id);
    setUrlFields(vendorUrlState(v));
  }, [form.vendor_id, vendors]);

  // 选择厂商：新建态自动填默认 URL（或 coding URL）；编辑态保留已保存值。
  // 协议不支持的字段清空，但把原值暂存到 _prev_*；切回支持协议时恢复原值，避免数据丢失。
  const handleVendorChange = (vendorId: string) => {
    const v = vendors.find((x) => x.id === vendorId);
    const { baseEnabled, anthEnabled, supportsCoding } = vendorUrlState(v);
    // 厂商支持 coding plan 时默认启用 coding 端点（在 setForm 外设置，保持 updater 纯函数）
    setUseCodingPlan(supportsCoding);
    setForm((f) => {
      if (!v) return { ...f, vendor_id: vendorId };
      const coding = supportsCoding;
      return {
        ...f,
        vendor_id: vendorId,
        base_url: !baseEnabled
          ? ""
          : isEdit
            ? f.base_url || f._prev_base_url || v.default_base_url || ""
            : coding
              ? v.coding_base_url ?? v.default_base_url ?? ""
              : v.default_base_url ?? "",
        _prev_base_url:
          !baseEnabled && f.base_url
            ? f.base_url
            : baseEnabled
              ? ""
              : f._prev_base_url,
        anthropic_base_url: !anthEnabled
          ? ""
          : isEdit
            ? f.anthropic_base_url || f._prev_anthropic_url || v.default_anthropic_url || ""
            : coding
              ? v.coding_anthropic_url ?? v.coding_base_url ?? v.default_anthropic_url ?? ""
              : v.default_anthropic_url ?? "",
        _prev_anthropic_url:
          !anthEnabled && f.anthropic_base_url
            ? f.anthropic_base_url
            : anthEnabled
              ? ""
              : f._prev_anthropic_url,
      };
    });
  };

  // 切换 Coding Plan：开启 → URL 锁定为 coding 端点；关闭 → 恢复厂商默认 URL
  const toggleCodingPlan = (enabled: boolean) => {
    const v = vendors.find((x) => x.id === form.vendor_id);
    if (!v) return;
    setUseCodingPlan(enabled);
    setForm((f) => ({
      ...f,
      base_url: enabled
        ? v.coding_base_url ?? v.default_base_url ?? ""
        : v.default_base_url ?? "",
      anthropic_base_url: enabled
        ? v.coding_anthropic_url ?? v.coding_base_url ?? v.default_anthropic_url ?? ""
        : v.default_anthropic_url ?? "",
    }));
  };

  const handleSubmit = async () => {
    setLoading(true);
    try {
      // 剔除内部暂存字段（_prev_*），只提交用户可见字段
      const { _prev_base_url: _pb, _prev_anthropic_url: _pa, ...payload } = form;
      if (isEdit && account?.id) {
        await accountApi.update(account.id, {
          ...payload,
          enabled: form.enabled ? 1 : 0,
          openai_compatible: form.openai_compatible ? 1 : 0,
        });
        toast.success(t("accounts.updated"));
      } else {
        const res = await accountApi.create({
          ...payload,
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
              onChange={handleVendorChange}
              options={vendors.map((v) => ({
                value: v.id,
                // 提供 coding URL 的厂商加标记，便于选厂商时就看出
                label:
                  v.coding_base_url || v.coding_anthropic_url
                    ? `${v.name} · Coding`
                    : v.name,
              }))}
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
              disabled={!urlFields.baseEnabled || useCodingPlan}
            />
          </div>
          <div>
            <label className={label}>{t("accounts.form.anthropicUrl")}</label>
            <Input
              value={form.anthropic_base_url}
              onChange={(v) => setForm((f) => ({ ...f, anthropic_base_url: v }))}
              placeholder="https://api.anthropic.com"
              disabled={!urlFields.anthEnabled || useCodingPlan}
            />
          </div>
        </div>

        {/* Coding Plan：厂商支持时可选，开启后默认 URL 禁用，走 coding 端点 */}
        {urlFields.supportsCoding && (
          <div className="rounded-xl border border-primary/20 bg-primary/5 p-3">
            <Switch
              label={t("accounts.form.codingPlan")}
              description={
                useCodingPlan ? t("accounts.form.codingLocked") : t("accounts.form.codingPlanDesc")
              }
              checked={useCodingPlan}
              onChange={toggleCodingPlan}
            />
          </div>
        )}

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
