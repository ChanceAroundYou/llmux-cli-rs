// 账户列表：管理上游账户（含 CSV 导出）
import React, { useEffect, useState, useCallback } from "react";
import { accountApi } from "@/api/accounts";
import { vendorApi } from "@/api/vendors";
import type { AccountPublic } from "@/types/account";
import type { Vendor } from "@/types/vendor";
import { Button } from "@/components/ui/Button";
import { Table } from "@/components/ui/Table";
import { Badge } from "@/components/ui/Badge";
import { ConfirmDialog } from "@/components/ui/ConfirmDialog";
import { AccountFormModal } from "./AccountForm";
import { useToast } from "@/hooks/useToast";
import { useT } from "@/i18n";
import { PageHeader } from "@/components/shared/PageHeader";
import { EmptyState } from "@/components/shared/EmptyState";
import { Download, Pencil, Trash2, Plus, Users, Inbox } from "lucide-react";

export const AccountList: React.FC = () => {
  const { t } = useT();
  const [accounts, setAccounts] = useState<AccountPublic[]>([]);
  const [vendors, setVendors] = useState<Vendor[]>([]);
  const [loading, setLoading] = useState(true);
  const [formOpen, setFormOpen] = useState(false);
  const [editingAccount, setEditingAccount] = useState<AccountPublic | null>(null);
  const [deleteTarget, setDeleteTarget] = useState<AccountPublic | null>(null);
  const [deleting, setDeleting] = useState(false);
  const toast = useToast();

  const fetchData = useCallback(async () => {
    setLoading(true);
    try {
      const [accs, vends] = await Promise.allSettled([accountApi.list(), vendorApi.list()]);
      if (accs.status === "fulfilled") setAccounts(accs.value);
      else toast.error(`Accounts: ${accs.reason?.message}`);
      if (vends.status === "fulfilled") setVendors(vends.value);
      else toast.error(`Vendors: ${vends.reason?.message}`);
    } catch (err) {
      toast.error(err instanceof Error ? err.message : t("accounts.loadFailed"));
    } finally {
      setLoading(false);
    }
  }, [t, toast]);

  useEffect(() => {
    fetchData();
  }, [fetchData]);

  const handleDelete = async () => {
    if (!deleteTarget?.id) return;
    setDeleting(true);
    try {
      await accountApi.remove(deleteTarget.id);
      toast.success(t("accounts.deleted"));
      setDeleteTarget(null);
      fetchData();
    } catch (err) {
      toast.error(err instanceof Error ? err.message : t("accounts.deleteFailed"));
    } finally {
      setDeleting(false);
    }
  };

  const getVendorName = (vendorId: string) =>
    vendors.find((v) => v.id === vendorId)?.name ?? vendorId;

  const columns = [
    {
      key: "name",
      title: t("accounts.col.name"),
      render: (row: AccountPublic) => (
        <div>
          <p className="font-medium text-card-foreground">{row.name}</p>
          {row.notes && <p className="text-xs text-muted-foreground">{row.notes}</p>}
        </div>
      ),
    },
    {
      key: "vendor_id",
      title: t("accounts.col.vendor"),
      render: (row: AccountPublic) => <Badge variant="info">{getVendorName(row.vendor_id)}</Badge>,
    },
    {
      key: "base_url",
      title: t("accounts.col.url"),
      render: (row: AccountPublic) => (
        <span className="font-mono text-xs text-muted-foreground">
          {row.base_url || row.anthropic_base_url || t("accounts.url.default")}
        </span>
      ),
    },
    {
      key: "weight",
      title: t("accounts.col.weight"),
      render: (row: AccountPublic) => <span className="font-mono">{row.weight}</span>,
    },
    {
      key: "enabled",
      title: t("accounts.col.status"),
      render: (row: AccountPublic) => (
        <Badge variant={row.enabled ? "success" : "neutral"}>
          {row.enabled ? t("accounts.status.enabled") : t("accounts.status.disabled")}
        </Badge>
      ),
    },
    {
      key: "actions",
      title: "",
      align: "right" as const,
      render: (row: AccountPublic) => (
        <div className="flex items-center justify-end gap-1">
          <Button size="sm" variant="ghost" title="CSV" aria-label="Export CSV" onClick={() => accountApi.exportCsv(row.id!)}>
            <Download className="h-3.5 w-3.5" />
          </Button>
          <Button
            size="sm"
            variant="ghost"
            title={t("common.edit")}
            aria-label={t("common.edit")}
            onClick={() => {
              setEditingAccount(row);
              setFormOpen(true);
            }}
          >
            <Pencil className="h-3.5 w-3.5" />
          </Button>
          <Button
            size="sm"
            variant="ghost"
            className="text-destructive-foreground hover:text-destructive"
            title={t("common.delete")}
            aria-label={t("common.delete")}
            onClick={() => setDeleteTarget(row)}
          >
            <Trash2 className="h-3.5 w-3.5" />
          </Button>
        </div>
      ),
    },
  ];

  return (
    <div className="animate-fade-in space-y-6">
      <PageHeader
        icon={Users}
        iconClass="bg-primary/20 text-primary-foreground"
        title={t("accounts.title")}
        description={t("accounts.desc")}
        actions={
          <Button
            onClick={() => {
              setEditingAccount(null);
              setFormOpen(true);
            }}
          >
            <Plus className="h-4 w-4" /> {t("accounts.add")}
          </Button>
        }
      />

      <Table
        columns={columns}
        data={accounts}
        rowKey={(row) => row.id ?? row.name}
        loading={loading}
        empty={<EmptyState icon={Inbox} title={t("accounts.empty")} />}
      />

      <AccountFormModal
        open={formOpen}
        account={editingAccount}
        vendors={vendors}
        onClose={() => {
          setFormOpen(false);
          setEditingAccount(null);
        }}
        onSuccess={() => {
          setFormOpen(false);
          setEditingAccount(null);
          fetchData();
        }}
      />

      <ConfirmDialog
        open={!!deleteTarget}
        title={t("accounts.delete.title")}
        message={t("accounts.delete.confirm", { name: deleteTarget?.name ?? "" })}
        danger
        loading={deleting}
        onConfirm={handleDelete}
        onCancel={() => setDeleteTarget(null)}
      />
    </div>
  );
};
