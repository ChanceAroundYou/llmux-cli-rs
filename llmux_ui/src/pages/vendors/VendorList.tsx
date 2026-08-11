// 厂商管理：内置 + 自定义，品牌色 Logo 卡片网格，编辑含 coding plan
import React, { useEffect, useState, useCallback } from "react";
import { vendorApi } from "@/api/vendors";
import type { Vendor, VendorCreatePayload } from "@/types/vendor";
import { Button } from "@/components/ui/Button";
import { Badge } from "@/components/ui/Badge";
import { Modal } from "@/components/ui/Modal";
import { Input } from "@/components/ui/Input";
import { Select } from "@/components/ui/Select";
import { Switch } from "@/components/ui/Switch";
import { ConfirmDialog } from "@/components/ui/ConfirmDialog";
import { useToast } from "@/hooks/useToast";
import { useT } from "@/i18n";
import { PageHeader } from "@/components/shared/PageHeader";
import { VendorLogo } from "@/components/vendors/VendorLogo";
import { cn } from "@/utils/helpers";
import { Building2 } from "lucide-react";

const PROTOCOLS = ["openai", "anthropic", "gemini", "custom"];

export const VendorList: React.FC = () => {
  const { t } = useT();
  const [vendors, setVendors] = useState<Vendor[]>([]);
  const [loading, setLoading] = useState(true);
  const [formOpen, setFormOpen] = useState(false);
  const [editing, setEditing] = useState<Vendor | null>(null);
  const [deleteTarget, setDeleteTarget] = useState<Vendor | null>(null);
  const [deleting, setDeleting] = useState(false);
  const [form, setForm] = useState<VendorCreatePayload>({
    id: "",
    name: "",
    protocol: "openai",
  });
  const [saving, setSaving] = useState(false);
  const toast = useToast();

  const fetchVendors = useCallback(async () => {
    setLoading(true);
    try {
      setVendors(await vendorApi.list());
    } catch {
      toast.error(t("vendors.loadFailed"));
    } finally {
      setLoading(false);
    }
  }, [t, toast]);

  useEffect(() => {
    fetchVendors();
  }, [fetchVendors]);

  const openCreate = () => {
    setEditing(null);
    setForm({ id: "", name: "", protocol: "openai" });
    setFormOpen(true);
  };

  const openEdit = (v: Vendor) => {
    setEditing(v);
    setForm({
      id: v.id,
      name: v.name,
      protocol: v.protocol,
      default_base_url: v.default_base_url,
      default_anthropic_url: v.default_anthropic_url,
      protocols: v.protocols,
      openai_responses: v.openai_responses,
      coding_plan: v.coding_plan,
      coding_base_url: v.coding_base_url,
      coding_anthropic_url: v.coding_anthropic_url,
    });
    setFormOpen(true);
  };

  const handleSave = async () => {
    setSaving(true);
    try {
      if (editing) {
        await vendorApi.update(editing.id, form);
        toast.success(t("vendors.updated"));
      } else {
        await vendorApi.create(form);
        toast.success(t("vendors.created"));
      }
      setFormOpen(false);
      fetchVendors();
    } catch (err) {
      toast.error(err instanceof Error ? err.message : t("vendors.saveFailed"));
    } finally {
      setSaving(false);
    }
  };

  const handleDelete = async () => {
    if (!deleteTarget) return;
    setDeleting(true);
    try {
      await vendorApi.remove(deleteTarget.id);
      toast.success(t("vendors.deleted"));
      setDeleteTarget(null);
      fetchVendors();
    } catch (err) {
      toast.error(err instanceof Error ? err.message : t("vendors.deleteFailed"));
    } finally {
      setDeleting(false);
    }
  };

  return (
    <div className="animate-fade-in space-y-6">
      <PageHeader
        icon={Building2}
        iconClass="bg-accent/50 text-accent-foreground"
        title={t("vendors.title")}
        description={t("vendors.desc")}
        actions={<Button onClick={openCreate}>+ {t("vendors.add")}</Button>}
      />

      {loading ? (
        <div className="space-y-2 p-4">
          {Array.from({ length: 6 }).map((_, i) => (
            <div key={i} className="h-28 animate-pulse rounded-xl bg-muted" />
          ))}
        </div>
      ) : (
        <div className="grid grid-cols-1 gap-4 sm:grid-cols-2 xl:grid-cols-3">
          {vendors.map((v) => (
            <div
              key={v.id}
              className="flex flex-col gap-3 rounded-xl border border-border bg-card p-4 transition-all hover:border-primary/30 hover:shadow-soft"
            >
              <div className="flex items-start gap-3">
                <VendorLogo id={v.id} name={v.name} size={44} />
                <div className="min-w-0 flex-1">
                  <div className="flex items-center gap-2">
                    <span className="truncate font-semibold text-card-foreground">
                      {v.name}
                    </span>
                    {v.builtin === 1 && (
                      <Badge variant="neutral">{t("vendors.builtin")}</Badge>
                    )}
                  </div>
                  <div className="truncate font-mono text-xs text-muted-foreground">
                    {v.id}
                  </div>
                </div>
              </div>

              <div className="flex flex-wrap gap-1">
                {v.protocols.map((p) => (
                  <Badge key={p} variant="info">
                    {p}
                  </Badge>
                ))}
                {v.openai_responses && <Badge variant="success">responses</Badge>}
                {v.coding_plan === 1 && <Badge variant="warning">coding</Badge>}
              </div>

              <div className="truncate font-mono text-xs text-muted-foreground">
                {v.default_base_url || "—"}
              </div>

              <div className="mt-auto flex gap-1 border-t border-border/60 pt-2">
                <Button size="sm" variant="ghost" onClick={() => openEdit(v)}>
                  {t("common.edit")}
                </Button>
                <Button
                  size="sm"
                  variant="ghost"
                  className="text-destructive-foreground hover:text-destructive"
                  disabled={v.builtin === 1}
                  onClick={() => setDeleteTarget(v)}
                >
                  {t("common.delete")}
                </Button>
              </div>
            </div>
          ))}
        </div>
      )}

      {/* Form Modal */}
      <Modal
        open={formOpen}
        onClose={() => setFormOpen(false)}
        title={editing ? t("vendors.form.titleEdit") : t("vendors.form.titleAdd")}
        size="lg"
        footer={
          <>
            <Button variant="secondary" onClick={() => setFormOpen(false)}>
              {t("common.cancel")}
            </Button>
            <Button loading={saving} onClick={handleSave}>
              {editing ? t("common.save") : t("common.create")}
            </Button>
          </>
        }
      >
        <div className="space-y-4">
          <div className="grid grid-cols-2 gap-4">
            <div>
              <label className="mb-1 block text-sm font-medium text-card-foreground">
                {t("vendors.form.id")} *
              </label>
              <Input
                value={form.id}
                onChange={(v) => setForm((f) => ({ ...f, id: v }))}
                disabled={!!editing}
                placeholder="my-vendor"
              />
            </div>
            <div>
              <label className="mb-1 block text-sm font-medium text-card-foreground">
                {t("vendors.form.name")} *
              </label>
              <Input
                value={form.name}
                onChange={(v) => setForm((f) => ({ ...f, name: v }))}
                placeholder="My Vendor"
              />
            </div>
          </div>
          <div className="grid grid-cols-2 gap-4">
            <div>
              <label className="mb-1 block text-sm font-medium text-card-foreground">
                {t("vendors.form.primaryProtocol")}
              </label>
              <Select
                value={form.protocol ?? "openai"}
                onChange={(v) => setForm((f) => ({ ...f, protocol: v }))}
                options={PROTOCOLS.map((p) => ({ value: p, label: p }))}
              />
            </div>
            <div>
              <label className="mb-1 block text-sm font-medium text-card-foreground">
                {t("vendors.form.defaultUrl")}
              </label>
              <Input
                value={form.default_base_url ?? ""}
                onChange={(v) => setForm((f) => ({ ...f, default_base_url: v || null }))}
                placeholder="https://..."
              />
            </div>
          </div>
          <div className="grid grid-cols-2 gap-4">
            <div>
              <label className="mb-1 block text-sm font-medium text-card-foreground">
                {t("vendors.form.anthropicUrl")}
              </label>
              <Input
                value={form.default_anthropic_url ?? ""}
                onChange={(v) => setForm((f) => ({ ...f, default_anthropic_url: v || null }))}
              />
            </div>
            <div className="flex items-end">
              <Switch
                label={t("vendors.form.responses")}
                checked={form.openai_responses ?? true}
                onChange={(v) => setForm((f) => ({ ...f, openai_responses: v }))}
              />
            </div>
          </div>

          {/* Coding Plan */}
          <div className="rounded-xl border border-border bg-muted/40 p-4">
            <Switch
              label={t("vendors.form.codingPlan")}
              checked={form.coding_plan === 1}
              onChange={(v) => setForm((f) => ({ ...f, coding_plan: v ? 1 : 0 }))}
            />
            <div className={cn("mt-3 grid grid-cols-2 gap-4 transition-opacity", form.coding_plan === 1 ? "opacity-100" : "pointer-events-none opacity-40")}>
              <div>
                <label className="mb-1 block text-sm font-medium text-card-foreground">
                  {t("vendors.form.codingUrl")}
                </label>
                <Input
                  value={form.coding_base_url ?? ""}
                  onChange={(v) => setForm((f) => ({ ...f, coding_base_url: v || null }))}
                />
              </div>
              <div>
                <label className="mb-1 block text-sm font-medium text-card-foreground">
                  {t("vendors.form.codingAnthropicUrl")}
                </label>
                <Input
                  value={form.coding_anthropic_url ?? ""}
                  onChange={(v) => setForm((f) => ({ ...f, coding_anthropic_url: v || null }))}
                />
              </div>
            </div>
          </div>
        </div>
      </Modal>

      <ConfirmDialog
        open={!!deleteTarget}
        title={t("vendors.delete.title")}
        message={t("vendors.delete.confirm", { name: deleteTarget?.name ?? "" })}
        danger
        loading={deleting}
        onConfirm={handleDelete}
        onCancel={() => setDeleteTarget(null)}
      />
    </div>
  );
};
