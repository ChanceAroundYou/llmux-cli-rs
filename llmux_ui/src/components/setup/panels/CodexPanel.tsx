// 快速配置：Codex 配置面板（密钥选择 + 模型/wireApi/窗口 + auth.json 与 config.toml 预览）
import React, { useEffect, useRef, useState } from "react";
import { Globe } from "lucide-react";
import { useT } from "@/i18n";
import { systemApi } from "@/api/system";
import { CopyButton } from "@/components/shared/CopyButton";
import { KeySelector } from "../KeySelector";
import { ModelRoleSelect } from "../ModelRoleSelect";
import { CodexSettingsPreview } from "../CodexSettingsPreview";
import { ApplyButton, type ApplyResult } from "../ApplyButton";
import { KeyReplacedNotice } from "../KeyReplacedNotice";
import { BackupHistory } from "../BackupHistory";
import { useToolBackups } from "../useToolBackups";
import { extractErrMsg, parseAllowedModels } from "../utils";
import { cn } from "@/utils/helpers";
import type { ApiKey } from "@/types/key";
import type { AliasResponse } from "@/types/model";

interface Props {
  keys: ApiKey[];
  aliases: AliasResponse[];
  gatewayUrl: string;
  currentAuth: Record<string, unknown> | null;
  currentToml: string | null;
  settingsExists: boolean;
  settingsLoading: boolean;
  onRefreshSettings: () => void;
  onSettingsApplied: (settings: Record<string, unknown>) => void;
}

interface TomlFields {
  model: string;
  wireApi: string;
  contextWindow: number;
  autoCompactLimit: number;
}

function extractFromToml(configToml: string): TomlFields {
  const model = (configToml.match(/model\s*=\s*"([^"]+)"/) ?? [])[1] ?? "";
  const wireApi = (configToml.match(/wire_api\s*=\s*"([^"]+)"/) ?? [])[1] ?? "";
  const ctxMatch = configToml.match(/model_context_window\s*=\s*(\d+)/);
  const contextWindow = ctxMatch ? parseInt(ctxMatch[1], 10) : 0;
  const compactMatch = configToml.match(/model_auto_compact_token_limit\s*=\s*(\d+)/);
  const autoCompactLimit = compactMatch ? parseInt(compactMatch[1], 10) : 0;
  return { model, wireApi, contextWindow, autoCompactLimit };
}

function isDirty(
  currentToml: string | null,
  model: string,
  wireApi: string,
  contextWindow: number,
  autoCompactLimit: number,
) {
  if (!currentToml) return false;
  const cur = extractFromToml(currentToml);
  return (
    cur.model !== model ||
    cur.wireApi !== wireApi ||
    (contextWindow > 0 && cur.contextWindow !== contextWindow) ||
    (autoCompactLimit > 0 && cur.autoCompactLimit !== autoCompactLimit)
  );
}

export const CodexPanel: React.FC<Props> = ({
  keys,
  aliases,
  gatewayUrl,
  currentAuth,
  currentToml,
  settingsExists,
  settingsLoading,
  onRefreshSettings,
  onSettingsApplied,
}) => {
  const { t } = useT();
  const backups = useToolBackups("codex");
  const skipKeyClear = useRef(1); // mount 时跳过首次 key 切换清空

  const [selectedKeyId, setSelectedKeyId] = useState<number | "">(
    () => keys[0]?.id ?? "",
  );
  const [model, setModel] = useState(() => extractFromToml(currentToml ?? "").model);
  const [wireApi, setWireApi] = useState<"responses" | "chat">(
    () => (extractFromToml(currentToml ?? "").wireApi as "responses" | "chat") || "responses",
  );
  const [contextWindow, setContextWindow] = useState(
    () => extractFromToml(currentToml ?? "").contextWindow,
  );
  const [autoCompactLimit, setAutoCompactLimit] = useState(
    () => extractFromToml(currentToml ?? "").autoCompactLimit,
  );

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
    setWireApi("responses");
    setContextWindow(0);
    setAutoCompactLimit(0);
    setApplyResult(null);
    setKeyReplacedNotice(null);
  }, [selectedKeyId]);

  // 还原回填：auth.json 的 key 自动匹配或替换，config.toml 提取字段
  useEffect(() => {
    if (!pendingFillContent || keys.length === 0) return;
    const content = pendingFillContent;
    setPendingFillContent(null);

    const configToml = typeof content.configToml === "string" ? content.configToml : "";
    skipKeyClear.current += 1;
    let notice: string | null = null;
    const auth = (content.auth ?? {}) as Record<string, unknown>;
    const backupApiKey =
      typeof auth.OPENAI_API_KEY === "string" ? auth.OPENAI_API_KEY : "";
    const matchedKey = backupApiKey ? keys.find((k) => k.key === backupApiKey) : undefined;
    const fallback = keys.find((k) => k.id === selectedKeyId) ?? keys[0];
    if (matchedKey) {
      setSelectedKeyId(matchedKey.id ?? "");
    } else if (fallback) {
      setSelectedKeyId(fallback.id ?? "");
      notice = t("setup.keyReplaced", { name: fallback.name });
    }
    const extracted = extractFromToml(configToml);
    if (extracted.model) setModel(extracted.model);
    if (extracted.wireApi) setWireApi(extracted.wireApi as "responses" | "chat");
    if (extracted.contextWindow) setContextWindow(extracted.contextWindow);
    if (extracted.autoCompactLimit) setAutoCompactLimit(extracted.autoCompactLimit);
    setKeyReplacedNotice(notice);
    setApplyResult(null);
  }, [pendingFillContent, keys, selectedKeyId, t]);

  // 预览由后端生成：与 apply 共用同一套 patch 逻辑，只算不写，天然一致
  const [preview, setPreview] = useState<{
    auth: Record<string, unknown> | null;
    configToml: string;
  } | null>(null);

  useEffect(() => {
    if (!selectedKey) {
      setPreview(null);
      return;
    }
    let cancelled = false;
    // 防抖 300ms：数字输入框连续改动时只发最后一次预览请求，避免每敲一键都请求
    const timer = setTimeout(() => {
      systemApi
        .previewCodexSettings({
          apiBaseUrl: `${gatewayUrl}/v1`,
          apiKey: selectedKey.key,
          model: model || undefined,
          reviewModel: model || undefined,
          wireApi,
          contextWindow: contextWindow || undefined,
          autoCompactLimit: autoCompactLimit || undefined,
        })
        .then((res) => {
          if (!cancelled) setPreview(res);
        })
        .catch(() => {
          // 预览失败静默：保留上一次结果，diff 不误判
        });
    }, 300);
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [selectedKey, gatewayUrl, model, wireApi, contextWindow, autoCompactLimit]);

  const handleApply = async () => {
    if (!selectedKey) return;
    setApplying(true);
    setApplyResult(null);
    try {
      const data = await systemApi.applyCodexSettings({
        apiBaseUrl: `${gatewayUrl}/v1`,
        apiKey: selectedKey.key,
        model: model || undefined,
        reviewModel: model || undefined,
        wireApi,
        contextWindow: contextWindow || undefined,
        autoCompactLimit: autoCompactLimit || undefined,
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
      isDirty(currentToml, model, wireApi, contextWindow, autoCompactLimit),
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
                envKey="codex_model"
                models={allowedModelsList}
                value={model}
                onChange={setModel}
              />
              <div className="space-y-1.5">
                <label className="text-xs font-bold uppercase text-muted-foreground">
                  {t("setup.wireApi")}
                </label>
                <div className="flex overflow-hidden rounded-lg border border-border text-xs font-semibold">
                  <button
                    type="button"
                    onClick={() => setWireApi("responses")}
                    className={cn(
                      "flex-1 px-3 py-1.5 transition-colors",
                      wireApi === "responses"
                        ? "bg-primary text-primary-foreground"
                        : "hover:bg-muted",
                    )}
                  >
                    responses
                  </button>
                  <button
                    type="button"
                    onClick={() => setWireApi("chat")}
                    className={cn(
                      "flex-1 px-3 py-1.5 transition-colors",
                      wireApi === "chat"
                        ? "bg-primary text-primary-foreground"
                        : "hover:bg-muted",
                    )}
                  >
                    chat
                  </button>
                </div>
                <p className="text-xs text-muted-foreground">
                  {t("setup.wireApiHint")}
                </p>
              </div>
              <div className="grid grid-cols-2 gap-3">
                <div className="space-y-1">
                  <label className="text-xs font-semibold uppercase text-muted-foreground">
                    {t("setup.contextWindow")}
                  </label>
                  <input
                    type="number"
                    value={contextWindow || ""}
                    onChange={(e) => setContextWindow(parseInt(e.target.value, 10) || 0)}
                    placeholder="1000000"
                    className="w-full rounded-lg border border-border bg-card px-3 py-1.5 font-mono text-xs focus:border-primary focus:outline-none"
                  />
                  <p className="text-xs text-muted-foreground/70">
                    {t("setup.contextWindowHint")}
                  </p>
                </div>
                <div className="space-y-1">
                  <label className="text-xs font-semibold uppercase text-muted-foreground">
                    {t("setup.autoCompactLimit")}
                  </label>
                  <input
                    type="number"
                    value={autoCompactLimit || ""}
                    onChange={(e) => setAutoCompactLimit(parseInt(e.target.value, 10) || 0)}
                    placeholder="900000"
                    className="w-full rounded-lg border border-border bg-card px-3 py-1.5 font-mono text-xs focus:border-primary focus:outline-none"
                  />
                  <p className="text-xs text-muted-foreground/70">
                    {t("setup.autoCompactLimitHint")}
                  </p>
                </div>
              </div>
              <p className="text-xs leading-relaxed text-muted-foreground">
                {t("setup.codexModelRolesHint")}
              </p>
            </div>
          )}

          <div className="rounded-lg border border-primary/10 bg-primary/5 p-3">
            <p className="text-xs leading-relaxed text-primary/80">
              {t("setup.codexEndpointHint")}
            </p>
          </div>
          <ApplyButton
            selectedKey={!!selectedKey}
            applying={applying}
            settingsExists={settingsExists}
            applyResult={applyResult}
            onApply={handleApply}
            applyLabel={t("setup.codexApplyBtn")}
            initLabel={t("setup.codexInitBtn")}
          />
        </div>

        <div className="space-y-3">
          <CodexSettingsPreview
            currentAuth={currentAuth}
            previewAuth={preview?.auth ?? null}
            currentToml={currentToml}
            previewToml={preview?.configToml ?? null}
            exists={settingsExists}
            loading={settingsLoading}
            onRefresh={onRefreshSettings}
          />
          <div className="flex items-center gap-3 rounded-xl border border-border bg-card px-4 py-2.5 text-xs text-muted-foreground">
            <Globe size={13} className="shrink-0 text-primary" />
            <span>
              {t("setup.gatewayUrl")}{" "}
              <span className="font-mono text-foreground">{gatewayUrl}/v1</span>
            </span>
            <CopyButton text={`${gatewayUrl}/v1`} className="ml-auto" />
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
