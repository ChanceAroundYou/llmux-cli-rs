// 密钥管理：网关密钥 + 白名单 + 一次性明文展示
import React, { useEffect, useState, useCallback } from "react";
import { keyApi } from "@/api/keys";
import type { ApiKey } from "@/types/key";
import { Button } from "@/components/ui/Button";
import { Table } from "@/components/ui/Table";
import { Badge } from "@/components/ui/Badge";
import { Modal } from "@/components/ui/Modal";
import { Input } from "@/components/ui/Input";
import { ConfirmDialog } from "@/components/ui/ConfirmDialog";
import { CopyButton } from "@/components/shared/CopyButton";
import { useToast } from "@/hooks/useToast";
import { useT } from "@/i18n";
import { PageHeader } from "@/components/shared/PageHeader";
import { EmptyState } from "@/components/shared/EmptyState";
import { formatTimestamp } from "@/utils/format";
import { KeyRound } from "lucide-react";

export const KeyList: React.FC = () => {
  const { t } = useT();
  const [keys, setKeys] = useState<ApiKey[]>([]);
  const [loading, setLoading] = useState(true);
  const [createOpen, setCreateOpen] = useState(false);
  const [newKeyName, setNewKeyName] = useState("");
  const [newKeyModels, setNewKeyModels] = useState("");
  const [creating, setCreating] = useState(false);
  const [createdKey, setCreatedKey] = useState<string | null>(null);
  const [deleteTarget, setDeleteTarget] = useState<ApiKey | null>(null);
  const [deleting, setDeleting] = useState(false);
  const toast = useToast();

  const fetchKeys = useCallback(async () => {
    setLoading(true);
    try {
      setKeys(await keyApi.list());
    } catch {
      toast.error(t("keys.loadFailed"));
    } finally {
      setLoading(false);
    }
  }, [t, toast]);

  useEffect(() => {
    fetchKeys();
  }, [fetchKeys]);

  const handleCreate = async () => {
    setCreating(true);
    try {
      const payload: { name?: string; allowed_models?: string | string[] } = {};
      if (newKeyName.trim()) payload.name = newKeyName.trim();
      if (newKeyModels.trim() && newKeyModels.trim() !== "*") {
        payload.allowed_models = newKeyModels.split(",").map((s) => s.trim()).filter(Boolean);
      }
      const res = await keyApi.create(payload);
      setCreatedKey(res.key);
      toast.success(t("keys.created"));
      fetchKeys();
    } catch (err) {
      toast.error(err instanceof Error ? err.message : t("keys.createFailed"));
    } finally {
      setCreating(false);
    }
  };

  const handleDelete = async () => {
    if (!deleteTarget?.id) return;
    setDeleting(true);
    try {
      await keyApi.remove(deleteTarget.id);
      toast.success(t("keys.deleted"));
      setDeleteTarget(null);
      fetchKeys();
    } catch {
      toast.error(t("keys.deleteFailed"));
    } finally {
      setDeleting(false);
    }
  };

  const renderAllowedModels = (val: string | string[]) => {
    if (val === "*") return <Badge variant="success">{t("keys.allModels")}</Badge>;
    const models = Array.isArray(val) ? val : [val];
    return (
      <div className="flex flex-wrap gap-1">
        {models.slice(0, 3).map((m) => (
          <Badge key={m} variant="info">
            {m}
          </Badge>
        ))}
        {models.length > 3 && <Badge variant="neutral">+{models.length - 3}</Badge>}
      </div>
    );
  };

  const columns = [
    {
      key: "name",
      title: t("keys.col.name"),
      render: (row: ApiKey) => <span className="font-medium text-card-foreground">{row.name}</span>,
    },
    {
      key: "key",
      title: t("keys.col.key"),
      render: (row: ApiKey) => (
        <div className="flex items-center gap-2">
          <code className="font-mono text-xs text-muted-foreground">{row.key.slice(0, 20)}...</code>
          <CopyButton text={row.key} />
        </div>
      ),
    },
    {
      key: "allowed_models",
      title: t("keys.col.models"),
      render: (row: ApiKey) => renderAllowedModels(row.allowed_models),
    },
    {
      key: "enabled",
      title: t("keys.col.status"),
      render: (row: ApiKey) => (
        <Badge variant={row.enabled ? "success" : "neutral"}>
          {row.enabled ? t("keys.status.active") : t("keys.status.disabled")}
        </Badge>
      ),
    },
    {
      key: "last_used_at",
      title: t("keys.col.lastUsed"),
      render: (row: ApiKey) => (
        <span className="text-xs text-muted-foreground">
          {row.last_used_at ? formatTimestamp(new Date(row.last_used_at).getTime() / 1000) : t("keys.never")}
        </span>
      ),
    },
    {
      key: "actions",
      title: "",
      align: "right" as const,
      render: (row: ApiKey) => (
        <Button
          size="sm"
          variant="ghost"
          className="text-destructive-foreground hover:text-destructive"
          onClick={() => setDeleteTarget(row)}
        >
          {t("common.delete")}
        </Button>
      ),
    },
  ];

  return (
    <div className="animate-fade-in space-y-6">
      <PageHeader
        icon={KeyRound}
        iconClass="bg-warning/25 text-warning-foreground"
        title={t("keys.title")}
        description={t("keys.desc")}
        actions={<Button onClick={() => setCreateOpen(true)}>+ {t("keys.create")}</Button>}
      />

      <Table
        columns={columns}
        data={keys}
        rowKey={(r) => r.id ?? r.key}
        loading={loading}
        empty={<EmptyState icon={KeyRound} title={t("keys.title")} description={t("keys.create")} />}
      />

      {/* Create Modal */}
      <Modal
        open={createOpen}
        onClose={() => {
          setCreateOpen(false);
          setCreatedKey(null);
          setNewKeyName("");
          setNewKeyModels("");
        }}
        title={t("keys.create.title")}
        size="sm"
        footer={
          createdKey ? (
            <Button
              onClick={() => {
                setCreateOpen(false);
                setCreatedKey(null);
              }}
            >
              {t("keys.create.done")}
            </Button>
          ) : (
            <>
              <Button variant="secondary" onClick={() => setCreateOpen(false)}>
                {t("common.cancel")}
              </Button>
              <Button loading={creating} onClick={handleCreate}>
                {t("common.create")}
              </Button>
            </>
          )
        }
      >
        {createdKey ? (
          <div className="space-y-3">
            <p className="text-sm text-muted-foreground">{t("keys.create.newKey")}</p>
            <div className="flex items-center gap-2 rounded-xl border border-success/30 bg-success/10 p-3">
              <code className="flex-1 break-all font-mono text-sm text-success-foreground">
                {createdKey}
              </code>
              <CopyButton text={createdKey} />
            </div>
            <p className="text-xs text-warning-foreground">⚠️ {t("keys.create.warning")}</p>
          </div>
        ) : (
          <div className="space-y-4">
            <div>
              <label className="mb-1 block text-sm font-medium text-card-foreground">
                {t("keys.form.name")}
              </label>
              <Input value={newKeyName} onChange={setNewKeyName} placeholder="My App Key" />
            </div>
            <div>
              <label className="mb-1 block text-sm font-medium text-card-foreground">
                {t("keys.form.models")}
              </label>
              <Input
                value={newKeyModels}
                onChange={setNewKeyModels}
                placeholder={t("keys.form.modelsPlaceholder")}
              />
              <p className="mt-1 text-xs text-muted-foreground">{t("keys.form.modelsHint")}</p>
            </div>
          </div>
        )}
      </Modal>

      <ConfirmDialog
        open={!!deleteTarget}
        title={t("keys.delete.title")}
        message={t("keys.delete.confirm", { name: deleteTarget?.name ?? "" })}
        danger
        loading={deleting}
        onConfirm={handleDelete}
        onCancel={() => setDeleteTarget(null)}
      />
    </div>
  );
};
