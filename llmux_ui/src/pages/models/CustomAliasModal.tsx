// 自定义别名：选账户 → 手输模型 → 先验证连通性 → 通过才可保存
import React, { useState, useEffect } from "react";
import type { AvailableModel } from "@/types/model";
import type { AccountPublic } from "@/types/account";
import { modelApi } from "@/api/models";
import { Modal } from "@/components/ui/Modal";
import { Button } from "@/components/ui/Button";
import { Input } from "@/components/ui/Input";
import { Select } from "@/components/ui/Select";
import { useT } from "@/i18n";
import { cn } from "@/utils/helpers";
import { Loader2, Zap, CheckCircle2, XCircle } from "lucide-react";

interface Props {
  open: boolean;
  models: AvailableModel[];
  accounts: AccountPublic[];
  saving: boolean;
  onClose: () => void;
  onSubmit: (payload: {
    alias: string;
    target_model: string;
    vendor_id?: string;
    account_ids?: number[];
  }) => void;
}

const labelCls = "mb-1 block text-sm font-medium text-card-foreground";

export const CustomAliasModal: React.FC<Props> = ({
  open,
  models,
  accounts,
  saving,
  onClose,
  onSubmit,
}) => {
  const { t } = useT();
  const [accountId, setAccountId] = useState("");
  const [alias, setAlias] = useState("");
  const [target, setTarget] = useState("");
  const [isVerifying, setIsVerifying] = useState(false);
  const [verifyResult, setVerifyResult] = useState<{
    success: boolean;
    latency?: number;
    error?: string;
  } | null>(null);

  useEffect(() => {
    if (open) {
      setAccountId("");
      setAlias("");
      setTarget("");
      setVerifyResult(null);
    }
  }, [open]);

  const account = accounts.find((a) => a.id === Number(accountId));
  const datalistOptions = models.filter((m) => !account || m.owned_by === account.vendor_id);

  const handleVerify = async () => {
    if (!target.trim() || !account) return;
    setIsVerifying(true);
    setVerifyResult(null);
    try {
      const res = await modelApi.test({
        model: target.trim(),
        vendorId: account.vendor_id,
        accountId: account.id,
      });
      setVerifyResult({ success: res.success, latency: res.latency, error: res.error ?? undefined });
    } catch (err) {
      setVerifyResult({
        success: false,
        error: err instanceof Error ? err.message : t("aliases.form.verifyFailed"),
      });
    } finally {
      setIsVerifying(false);
    }
  };

  const submit = () => {
    if (!alias.trim() || !target.trim()) return;
    onSubmit({
      alias: alias.trim(),
      target_model: target.trim(),
      // 自定义别名归属选中的账户厂商
      vendor_id: account?.vendor_id,
      account_ids: account?.id != null ? [account.id] : undefined,
    });
  };

  return (
    <Modal
      open={open}
      onClose={onClose}
      title={t("aliases.form.customTitle")}
      size="md"
      footer={
        <>
          <Button variant="secondary" onClick={onClose}>
            {t("common.cancel")}
          </Button>
          <Button loading={saving} disabled={!verifyResult?.success} onClick={submit}>
            {t("common.save")}
          </Button>
        </>
      }
    >
      <div className="space-y-4">
        <div>
          <label className={labelCls}>{t("aliases.form.selectAccount")} *</label>
          <Select
            value={accountId}
            onChange={(v) => {
              setAccountId(v);
              setTarget("");
              setVerifyResult(null);
            }}
            options={accounts
              .filter((a) => a.enabled)
              .map((a) => ({ value: String(a.id), label: `[${a.vendor_id}] ${a.name}` }))}
            placeholder={t("aliases.form.selectAccountPlaceholder")}
          />
        </div>

        <div>
          <label className={labelCls}>{t("aliases.form.alias")} *</label>
          <Input value={alias} onChange={setAlias} placeholder="my-gpt4" />
        </div>

        <div>
          <label className={labelCls}>{t("aliases.form.manualModel")} *</label>
          <Input
            value={target}
            onChange={(v) => {
              setTarget(v);
              setVerifyResult(null);
            }}
            placeholder="gpt-4o"
            list="custom-model-options"
          />
          <datalist id="custom-model-options">
            {datalistOptions.map((m) => (
              <option key={m.id} value={m.id} />
            ))}
          </datalist>
          <p className="mt-1 text-xs text-muted-foreground">{t("aliases.form.manualModelHint")}</p>
        </div>

        <div className="space-y-2">
          <Button
            type="button"
            variant="outline"
            onClick={handleVerify}
            disabled={isVerifying || !target.trim() || !accountId}
            className="border-warning/40 text-warning-foreground hover:bg-warning/10"
          >
            {isVerifying ? (
              <Loader2 className="h-4 w-4 animate-spin" />
            ) : (
              <Zap className="h-4 w-4" />
            )}
            {isVerifying ? t("aliases.form.verifying") : t("aliases.form.verify")}
          </Button>
          {verifyResult && (
            <div
              className={cn(
                "flex items-center gap-2 rounded-xl border p-3 text-sm font-medium animate-fade-in",
                verifyResult.success
                  ? "border-success/30 bg-success/10 text-success-foreground"
                  : "border-destructive/30 bg-destructive/10 text-destructive-foreground",
              )}
            >
              {verifyResult.success ? (
                <CheckCircle2 className="h-4 w-4 shrink-0" />
              ) : (
                <XCircle className="h-4 w-4 shrink-0" />
              )}
              {verifyResult.success
                ? `${t("aliases.form.verifySuccess")}${verifyResult.latency ? ` · ${(verifyResult.latency / 1000).toFixed(1)}s` : ""}`
                : verifyResult.error || t("aliases.form.verifyFailed")}
            </div>
          )}
          <p className="text-xs text-muted-foreground">{t("aliases.form.verifiedOnly")}</p>
        </div>
      </div>
    </Modal>
  );
};
