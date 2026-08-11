// 模型健康：各账户/模型组合最新测试结果
import React, { useEffect, useState, useCallback } from "react";
import { modelApi } from "@/api/models";
import type { ModelHealthEntry } from "@/types/model";
import { Badge } from "@/components/ui/Badge";
import { Table } from "@/components/ui/Table";
import { Button } from "@/components/ui/Button";
import { StatusDot } from "@/components/shared/StatusDot";
import { formatTimestamp, formatLatency } from "@/utils/format";
import { useToast } from "@/hooks/useToast";
import { useT } from "@/i18n";
import { PageHeader } from "@/components/shared/PageHeader";
import { EmptyState } from "@/components/shared/EmptyState";
import { RefreshCw, HeartPulse } from "lucide-react";

export const ModelHealth: React.FC = () => {
  const { t } = useT();
  const [entries, setEntries] = useState<ModelHealthEntry[]>([]);
  const [loading, setLoading] = useState(true);
  const toast = useToast();

  const fetchHealth = useCallback(async () => {
    setLoading(true);
    try {
      setEntries(await modelApi.getHealth());
    } catch {
      toast.error(t("health.loadFailed"));
    } finally {
      setLoading(false);
    }
  }, [t, toast]);

  useEffect(() => {
    fetchHealth();
  }, [fetchHealth]);

  const columns = [
    {
      key: "account_name",
      title: t("health.col.account"),
      render: (row: ModelHealthEntry) => (
        <div className="flex items-center gap-2">
          <StatusDot status={row.success ? "healthy" : "down"} />
          <span className="font-medium text-card-foreground">
            {row.account_name ?? `Account #${row.account_id}`}
          </span>
        </div>
      ),
    },
    {
      key: "model",
      title: t("health.col.model"),
      render: (row: ModelHealthEntry) => <code className="text-xs text-muted-foreground">{row.model || "—"}</code>,
    },
    {
      key: "vendor_id",
      title: t("health.col.vendor"),
      render: (row: ModelHealthEntry) => (row.vendor_id ? <Badge variant="info">{row.vendor_id}</Badge> : "—"),
    },
    {
      key: "latency",
      title: t("health.col.latency"),
      render: (row: ModelHealthEntry) => <span className="font-mono text-sm">{formatLatency(row.latency)}</span>,
    },
    {
      key: "success",
      title: t("health.col.status"),
      render: (row: ModelHealthEntry) => (
        <Badge variant={row.success ? "success" : "danger"}>
          {row.success ? t("health.pass") : t("health.fail")}
        </Badge>
      ),
    },
    {
      key: "error",
      title: t("health.col.error"),
      render: (row: ModelHealthEntry) => (
        <span className="block max-w-[200px] truncate text-xs text-destructive-foreground">
          {row.error || "— "}
        </span>
      ),
    },
    {
      key: "last_checked",
      title: t("health.col.checked"),
      render: (row: ModelHealthEntry) => <span className="text-xs text-muted-foreground">{formatTimestamp(row.last_checked)}</span>,
    },
  ];

  return (
    <div className="animate-fade-in space-y-6">
      <PageHeader
        icon={HeartPulse}
        iconClass="bg-success/20 text-success-foreground"
        title={t("health.title")}
        description={t("health.desc")}
        actions={
          <Button variant="outline" onClick={fetchHealth} loading={loading}>
            <RefreshCw className="h-4 w-4" /> {t("common.refresh")}
          </Button>
        }
      />

      <Table
        columns={columns}
        data={entries}
        rowKey={(r) => `${r.account_id}-${r.model}`}
        loading={loading}
        empty={<EmptyState icon={HeartPulse} title={t("health.empty")} />}
      />
    </div>
  );
};
