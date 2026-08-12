// 导入 / 导出配置（马卡龙化）
import React, { useRef, useState } from "react";
import { settingsApi } from "@/api/settings";
import type { ImportResponse } from "@/types/settings";
import { Card } from "@/components/ui/Card";
import { Button } from "@/components/ui/Button";
import { useToast } from "@/hooks/useToast";
import { useT } from "@/i18n";

export const ImportExport: React.FC = () => {
  const { t } = useT();
  const fileRef = useRef<HTMLInputElement>(null);
  const [importing, setImporting] = useState(false);
  const [importResult, setImportResult] = useState<ImportResponse | null>(null);
  const toast = useToast();

  const handleExport = async () => {
    try {
      await settingsApi.exportConfig();
      toast.success(t("ie.export.success"));
    } catch {
      toast.error(t("ie.export.failed"));
    }
  };

  const handleImport = async (e: React.ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0];
    if (!file) return;
    setImporting(true);
    try {
      const text = await file.text();
      const config = JSON.parse(text);
      const res = await settingsApi.importConfig(config);
      setImportResult(res);
      toast.success(
        t("ie.import.result", {
          accounts: res.imported.accounts,
          aliases: res.imported.aliases,
          keys: res.imported.keys,
        }),
      );
    } catch (err) {
      toast.error(err instanceof Error ? err.message : t("ie.import.failed"));
    } finally {
      setImporting(false);
      if (fileRef.current) fileRef.current.value = "";
    }
  };

  return (
    <div className="grid grid-cols-1 gap-6 lg:grid-cols-2">
      <Card title={t("ie.export.title")} description={t("ie.export.desc")}>
        <Button onClick={handleExport}>↓ {t("ie.export.button")}</Button>
        <p className="mt-3 text-xs text-warning-foreground">⚠️ {t("ie.export.warning")}</p>
      </Card>

      <Card title={t("ie.import.title")} description={t("ie.import.desc")}>
        <input
          ref={fileRef}
          type="file"
          accept=".json"
          onChange={handleImport}
          className="hidden"
        />
        <Button variant="outline" onClick={() => fileRef.current?.click()} loading={importing}>
          ↑ {t("ie.import.button")}
        </Button>
        {importResult && (
          <div className="mt-3 rounded-xl border border-success/30 bg-success/10 p-3 text-sm text-success-foreground animate-fade-in">
            ✓{" "}
            {t("ie.import.result", {
              accounts: importResult.imported.accounts,
              aliases: importResult.imported.aliases,
              keys: importResult.imported.keys,
            })}
          </div>
        )}
      </Card>
    </div>
  );
};
