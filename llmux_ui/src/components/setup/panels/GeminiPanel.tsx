// 快速配置：Gemini CLI 配置面板（密钥选择 + 默认模型 + .env 与 settings.json 预览）
import React, { useEffect, useRef, useState } from "react";
import { Globe } from "lucide-react";
import { useT } from "@/i18n";
import { systemApi } from "@/api/system";
import { CopyButton } from "@/components/shared/CopyButton";
import { KeySelector } from "../KeySelector";
import { ModelRoleSelect } from "../ModelRoleSelect";
import { GeminiSettingsPreview } from "../GeminiSettingsPreview";
import { ApplyButton, type ApplyResult } from "../ApplyButton";
import { KeyReplacedNotice } from "../KeyReplacedNotice";
import { BackupHistory } from "../BackupHistory";
import { useToolBackups } from "../useToolBackups";
import { extractErrMsg, parseAllowedModels } from "../utils";
import type { ApiKey } from "@/types/key";
import type { AliasResponse } from "@/types/model";

interface Props {
  keys: ApiKey[];
  aliases: AliasResponse[];
  gatewayUrl: string;
  currentEnv: string | null;
  currentSettings: string | null;
  settingsExists: boolean;
  settingsLoading: boolean;
  onRefreshSettings: () => void;
  onSettingsApplied: (settings: Record<string, unknown>) => void;
}

// 从 settings.json 字符串提取 model.name
function extractFromSettings(settingsStr: string | null | undefined): string {
  if (!settingsStr) return "";
  try {
    const parsed = JSON.parse(settingsStr) as {
      model?: { name?: string };
    };
    return parsed.model?.name ?? "";
  } catch {
    return "";
  }
}

function isDirty(
  currentSettings: string | null,
  model: string,
) {
  if (!currentSettings) return false;
  return extractFromSettings(currentSettings) !== model;
}

export const GeminiPanel: React.FC<Props> = ({
  keys,
  aliases,
  gatewayUrl,
  currentEnv,
  currentSettings,
  settingsExists,
  settingsLoading,
  onRefreshSettings,
  onSettingsApplied,
}) => {
  const { t } = useT();
  const backups = useToolBackups("gemini");
  const skipKeyClear = useRef(1);

  const [selectedKeyId, setSelectedKeyId] = useState<number | "">(
    () => keys[0]?.id ?? "",
  );
  const [model, setModel] = useState(() => extractFromSettings(currentSettings));

  const [applying, setApplying] = useState(false);
  const [applyResult, setApplyResult] = useState<ApplyResult | null>(null);
  const [keyReplacedNotice, setKeyReplacedNotice] = useState<string | null>(null);
  const [pendingFillContent, setPendingFillContent] = useState<
    Record<string, unknown> | null
  >(null);

  const selectedKey = keys.find((k) => k.id === selectedKeyId);
  const allowedModelsList = selectedKey
    ? selectedKey.allowed_models === "*"
      ? aliases.map((a) => a.alias)
      : parseAllowedModels(selectedKey.allowed_models)
    : [];

  // key 切换时清空模型（mount 与还原通过 skipKeyClear 跳过）
  useEffect(() => {
    if (skipKeyClear.current > 0) {
      skipKeyClear.current--;
      return;
    }
    setModel("");
    setApplyResult(null);
    setKeyReplacedNotice(null);
  }, [selectedKeyId]);

  // 还原回填：.env 中提取 key 自动匹配或替换，settings.json 提取模型
  useEffect(() => {
    if (!pendingFillContent || keys.length === 0) return;
    const content = pendingFillContent;
    setPendingFillContent(null);

    skipKeyClear.current += 1;
    let notice: string | null = null;
    const envStr = typeof content.env === "string" ? content.env : "";
    const backupKeyMatch = envStr.match(/^GEMINI_API_KEY=(\S+)$/m);
    const backupApiKey = backupKeyMatch?.[1] ?? "";
    const matchedKey = backupApiKey
      ? keys.find((k) => k.key === backupApiKey)
      : undefined;
    const fallback = keys.find((k) => k.id === selectedKeyId) ?? keys[0];
    if (matchedKey) {
      setSelectedKeyId(matchedKey.id ?? "");
    } else if (fallback) {
      setSelectedKeyId(fallback.id ?? "");
      notice = t("setup.keyReplaced", { name: fallback.name });
    }
    const settingsStr = typeof content.settings === "string" ? content.settings : "";
    const extracted = extractFromSettings(settingsStr);
    if (extracted) setModel(extracted);
    setKeyReplacedNotice(notice);
    setApplyResult(null);
  }, [pendingFillContent, keys, selectedKeyId, t]);

  // 预览由后端生成：与 apply 共用同一套逻辑（.env 的 set_env_key + settings.json 的 model.name），
  // 只算不写，天然一致
  const [preview, setPreview] = useState<{ env: string; settings: string } | null>(null);

  useEffect(() => {
    if (!selectedKey) {
      setPreview(null);
      return;
    }
    let cancelled = false;
    systemApi
      .previewGeminiSettings({
        apiKey: selectedKey.key,
        gatewayUrl,
        model: model || undefined,
      })
      .then((res) => {
        if (!cancelled) setPreview(res);
      })
      .catch(() => {
        // 预览失败静默：保留上一次结果，diff 不误判
      });
    return () => {
      cancelled = true;
    };
  }, [selectedKey, gatewayUrl, model]);

  const handleApply = async () => {
    if (!selectedKey) return;
    setApplying(true);
    setApplyResult(null);
    try {
      const data = await systemApi.applyGeminiSettings({
        apiKey: selectedKey.key,
        gatewayUrl,
        model: model || undefined,
      });
      if (data.success) {
        setApplyResult({ success: true, backupPath: data.backupPath });
        if (data.settings) onSettingsApplied(data.settings);
        setKeyReplacedNotice(null);
        void backups.fetchBackups();
      } else {
        setApplyResult({ success: false, error: data.error });
      }
    } catch (err) {
      setApplyResult({ success: false, error: extractErrMsg(err) });
    } finally {
      setApplying(false);
    }
  };

  const onRestoreClick = (name: string) => {
    void backups.handleRestoreClick(
      name,
      isDirty(currentSettings, model),
      (content) => setPendingFillContent(content),
    );
  };

  return (
    <div className="space-y-5">
      <KeyReplacedNotice
        notice={keyReplacedNotice}
        onDismiss={() => setKeyReplacedNotice(null)}
      />

      <div className="grid grid-cols-1 gap-5 xl:grid-cols-2">
        <div className="space-y-4">
          <div className="space-y-2">
            <div className="text-xs font-semibold uppercase tracking-widest text-muted-foreground">
              {t("setup.step1")}
            </div>
            <KeySelector
              keys={keys}
              selectedKeyId={selectedKeyId}
              onSelect={(id) => setSelectedKeyId(id)}
            />
          </div>

          {allowedModelsList.length > 0 && (
            <div className="space-y-3 rounded-xl border border-border bg-card p-4">
              <div className="text-xs font-semibold uppercase tracking-widest text-muted-foreground">
                {t("setup.modelRoles")}
              </div>
              <ModelRoleSelect
                label={t("setup.defaultModel")}
                envKey="gemini_model"
                models={allowedModelsList}
                value={model}
                onChange={setModel}
              />
              <p className="text-xs leading-relaxed text-muted-foreground">
                {t("setup.geminiModelHint")}
              </p>
            </div>
          )}

          <div className="rounded-lg border border-primary/10 bg-primary/5 p-3">
            <p className="text-xs leading-relaxed text-primary/80">
              {t("setup.geminiEndpointHint")}
            </p>
          </div>
          <ApplyButton
            selectedKey={!!selectedKey}
            applying={applying}
            settingsExists={settingsExists}
            applyResult={applyResult}
            onApply={handleApply}
            applyLabel={t("setup.geminiApplyBtn")}
            initLabel={t("setup.geminiInitBtn")}
          />
        </div>

        <div className="space-y-3">
          <GeminiSettingsPreview
            currentEnv={currentEnv}
            previewEnv={preview?.env ?? null}
            currentSettings={currentSettings}
            previewSettings={preview?.settings ?? null}
            exists={settingsExists}
            loading={settingsLoading}
            onRefresh={onRefreshSettings}
          />
          <div className="flex items-center gap-3 rounded-xl border border-border bg-card px-4 py-2.5 text-xs text-muted-foreground">
            <Globe size={13} className="shrink-0 text-primary" />
            <span>
              {t("setup.gatewayUrl")}{" "}
              <span className="font-mono text-foreground">{gatewayUrl}</span>
            </span>
            <CopyButton text={gatewayUrl} className="ml-auto" />
          </div>
        </div>
      </div>

      <BackupHistory
        backups={backups.backups}
        backupsLoading={backups.backupsLoading}
        isRestoring={backups.isRestoring}
        pendingRestoreName={backups.pendingRestoreName}
        expandedBackup={backups.expandedBackup}
        backupContents={backups.backupContents}
        dirtyModalOpen={backups.dirtyModalOpen}
        deleteModalName={backups.deleteModalName}
        onToggleExpand={backups.toggleExpand}
        onRestoreClick={onRestoreClick}
        onDeleteClick={backups.setDeleteModalName}
        onDirtyModalClose={() => {
          backups.setDirtyModalOpen(false);
          backups.setPendingRestoreName(null);
        }}
        onDirtyModalConfirm={() => backups.confirmRestore((c) => setPendingFillContent(c))}
        onDeleteModalClose={() => backups.setDeleteModalName(null)}
        onDeleteConfirm={backups.handleDeleteConfirm}
      />
    </div>
  );
};
