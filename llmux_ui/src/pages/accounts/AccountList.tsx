// 账户列表：管理上游账户（含 CSV 导出）
import React, { useState, useRef } from "react";
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
import { useCachedData } from "@/hooks/useCachedData";
import { useT } from "@/i18n";
import { PageHeader } from "@/components/shared/PageHeader";
import { EmptyState } from "@/components/shared/EmptyState";
import { Download, Pencil, Trash2, Plus, Users, Inbox, Power, PowerOff } from "lucide-react";

// 账户页展示数据：账户列表 + 厂商列表（厂商用于渲染徽标名称）
interface AccountListData {
  accounts: AccountPublic[];
  vendors: Vendor[];
}

export const AccountList: React.FC = () => {
  const { t } = useT();
  const toast = useToast();
  // 账户+厂商合并缓存：切回本页直接展示旧数据，过期后后台刷新；快速请求不闪骨架
  const { data, showSkeleton, setData, refetch: fetchData } = useCachedData<AccountListData>(
    "accountList",
    async () => {
      const [accs, vends] = await Promise.allSettled([accountApi.list(), vendorApi.list()]);
      // 单项失败仍返回其余成功项，错误单独提示
      if (accs.status === "rejected") toast.error(`Accounts: ${accs.reason?.message}`);
      if (vends.status === "rejected") toast.error(`Vendors: ${vends.reason?.message}`);
      return {
        accounts: accs.status === "fulfilled" ? accs.value : [],
        vendors: vends.status === "fulfilled" ? vends.value : [],
      };
    },
    { ttlMs: 60_000 },
  );
  const accounts = data?.accounts ?? [];
  const vendors = data?.vendors ?? [];
  const [formOpen, setFormOpen] = useState(false);
  const [editingAccount, setEditingAccount] = useState<AccountPublic | null>(null);
  const [deleteTarget, setDeleteTarget] = useState<AccountPublic | null>(null);
  const [deleting, setDeleting] = useState(false);
  const [togglingId, setTogglingId] = useState<number | null>(null);
  // 同步守卫：跨渲染防重复点击（state 更新是异步的，同一帧两次点击会读到旧值）
  const togglingRef = useRef(false);

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

  // 快捷启用/停用：只传 enabled，后端 merge 保留其余字段。
  // 成功后本地更新该行状态（行内刷新），避免整表重拉导致闪屏。
  const handleToggle = async (row: AccountPublic) => {
    const id = row.id;
    if (id == null || togglingId != null || togglingRef.current) return;
    const nextEnabled = row.enabled ? 0 : 1;
    togglingRef.current = true;
    setTogglingId(id);
    try {
      await accountApi.update(id, { enabled: nextEnabled });
      // 本地更新该行并写回缓存，避免整表重拉导致闪屏
      setData((prev) => ({
        ...(prev ?? { accounts: [], vendors: [] }),
        accounts: (prev?.accounts ?? []).map((a) =>
          a.id === id ? { ...a, enabled: nextEnabled } : a,
        ),
      }));
      toast.success(nextEnabled ? t("accounts.enabledMsg") : t("accounts.disabledMsg"));
    } catch (err) {
      toast.error(err instanceof Error ? err.message : t("accounts.toggleFailed"));
    } finally {
      togglingRef.current = false;
      setTogglingId(null);
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
        <div className="flex items-center gap-1.5">
          <Badge variant={row.enabled ? "success" : "neutral"}>
            {row.enabled ? t("accounts.status.enabled") : t("accounts.status.disabled")}
          </Badge>
          {row.uses_coding === 1 && (
            <Badge variant="warning">{t("accounts.badge.coding")}</Badge>
          )}
        </div>
      ),
    },
    {
      key: "actions",
      title: "",
      align: "right" as const,
      render: (row: AccountPublic) => (
        <div className="flex items-center justify-end gap-1">
          <Button
            size="sm"
            variant="ghost"
            loading={togglingId === row.id}
            icon={row.enabled ? <Power className="h-3.5 w-3.5" /> : <PowerOff className="h-3.5 w-3.5" />}
            title={row.enabled ? t("accounts.status.disabled") : t("accounts.status.enabled")}
            aria-label={row.enabled ? t("accounts.status.disabled") : t("accounts.status.enabled")}
            className={
              row.enabled
                ? "text-destructive-foreground hover:text-destructive"
                : "text-success hover:text-success-foreground"
            }
            onClick={() => handleToggle(row)}
          />
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
    <div className="space-y-6">
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
        loading={showSkeleton}
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
