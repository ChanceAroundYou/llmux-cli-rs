// 快速配置：Claude Code 配置面板（密钥选择 + 模型角色 + diff 预览 + 备份）
import React, { useEffect, useMemo, useRef, useState } from "react";
import { Globe } from "lucide-react";
import { useT } from "@/i18n";
import { systemApi } from "@/api/system";
import { CopyButton } from "@/components/shared/CopyButton";
import { KeySelector } from "../KeySelector";
import { ModelRoleSelect } from "../ModelRoleSelect";
import { SettingsPreview } from "../SettingsPreview";
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
  currentSettings: Record<string, unknown> | null;
  settingsExists: boolean;
  settingsLoading: boolean;
  settingsFetched: boolean;
  onRefreshSettings: () => void;
  onSettingsApplied: (settings: Record<string, unknown>) => void;
}

function withLongContext(alias: string, enabled: boolean) {
  return alias && enabled ? `${alias}[1m]` : alias;
}

function parseModel(val: unknown): { alias: string; longContext: boolean } {
  const str = typeof val === "string" ? val : "";
  if (!str) return { alias: "", longContext: false };
  if (str.endsWith("[1m]")) return { alias: str.slice(0, -4), longContext: true };
  return { alias: str, longContext: false };
}

function isDirty(
  currentSettings: Record<string, unknown> | null,
  opus: string,
  sonnet: string,
  haiku: string,
  opus1m: boolean,
  sonnet1m: boolean,
  haiku1m: boolean,
) {
  if (!currentSettings) return false;
  const env = (currentSettings.env ?? {}) as Record<string, unknown>;
  if ((env.ANTHROPIC_DEFAULT_OPUS_MODEL ?? "") !== withLongContext(opus, opus1m))
    return true;
  if ((env.ANTHROPIC_DEFAULT_SONNET_MODEL ?? "") !== withLongContext(sonnet, sonnet1m))
    return true;
  if ((env.ANTHROPIC_DEFAULT_HAIKU_MODEL ?? "") !== withLongContext(haiku, haiku1m))
    return true;
  return false;
}

export const ClaudeCodePanel: React.FC<Props> = ({
  keys,
  aliases,
  gatewayUrl,
  currentSettings,
  settingsExists,
  settingsLoading,
  settingsFetched,
  onRefreshSettings,
  onSettingsApplied,
}) => {
  const { t } = useT();
  const backups = useToolBackups("claude");

  const initializedFromSettings = useRef(false);
  const restoringRef = useRef(false);
  const skipNextKeyCleanup = useRef(0);

  const [selectedKeyId, setSelectedKeyId] = useState<number | "">("");
  const [opusModel, setOpusModel] = useState("");
  const [sonnetModel, setSonnetModel] = useState("");
  const [haikuModel, setHaikuModel] = useState("");
  const [opus1m, setOpus1m] = useState(false);
  const [sonnet1m, setSonnet1m] = useState(false);
  const [haiku1m, setHaiku1m] = useState(false);

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

  // 初始化：settings 与 keys 就绪后一次性回填 key + 模型
  useEffect(() => {
    if (!settingsFetched || initializedFromSettings.current || keys.length === 0)
      return;
    initializedFromSettings.current = true;
    const env = (currentSettings?.env ?? {}) as Record<string, unknown>;

    skipNextKeyCleanup.current = 2;
    setSelectedKeyId(keys[0].id ?? "");

    const opus = parseModel(env.ANTHROPIC_DEFAULT_OPUS_MODEL);
    const sonnet = parseModel(env.ANTHROPIC_DEFAULT_SONNET_MODEL);
    const haiku = parseModel(env.ANTHROPIC_DEFAULT_HAIKU_MODEL);
    if (opus.alias) {
      setOpusModel(opus.alias);
      setOpus1m(opus.longContext);
    }
    if (sonnet.alias) {
      setSonnetModel(sonnet.alias);
      setSonnet1m(sonnet.longContext);
    }
    if (haiku.alias) {
      setHaikuModel(haiku.alias);
      setHaiku1m(haiku.longContext);
    }
  }, [settingsFetched, keys, currentSettings]);

  // key 切换时清空模型（跳过初始化与还原触发）
  useEffect(() => {
    if (!initializedFromSettings.current) return;
    if (skipNextKeyCleanup.current > 0) {
      skipNextKeyCleanup.current--;
      return;
    }
    if (restoringRef.current) {
      restoringRef.current = false;
      return;
    }
    setOpusModel("");
    setSonnetModel("");
    setHaikuModel("");
    setOpus1m(false);
    setSonnet1m(false);
    setHaiku1m(false);
    setApplyResult(null);
    setKeyReplacedNotice(null);
  }, [selectedKeyId]);

  // 首次加载且没有已有 settings 时默认选第一个 key
  useEffect(() => {
    if (keys.length > 0 && selectedKeyId === "" && !currentSettings) {
      setSelectedKeyId(keys[0].id ?? "");
    }
  }, [keys, currentSettings, selectedKeyId]);

  // 还原回填：备份内容写入表单，key 自动匹配或替换
  useEffect(() => {
    if (!pendingFillContent || keys.length === 0) return;
    const content = pendingFillContent;
    setPendingFillContent(null);

    const env = (content.env ?? {}) as Record<string, unknown>;
    const backupApiKey =
      typeof env.ANTHROPIC_AUTH_TOKEN === "string" ? env.ANTHROPIC_AUTH_TOKEN : "";
    restoringRef.current = true;
    let notice: string | null = null;
    const matchedKey = keys.find((k) => k.key === backupApiKey);
    const fallback = keys.find((k) => k.id === selectedKeyId) ?? keys[0];
    if (matchedKey) {
      setSelectedKeyId(matchedKey.id ?? "");
    } else if (fallback) {
      setSelectedKeyId(fallback.id ?? "");
      notice = t("setup.keyReplaced", { name: fallback.name });
    }
    const opus = parseModel(env.ANTHROPIC_DEFAULT_OPUS_MODEL);
    const sonnet = parseModel(env.ANTHROPIC_DEFAULT_SONNET_MODEL);
    const haiku = parseModel(env.ANTHROPIC_DEFAULT_HAIKU_MODEL);
    setOpusModel(opus.alias);
    setOpus1m(opus.longContext);
    setSonnetModel(sonnet.alias);
    setSonnet1m(sonnet.longContext);
    setHaikuModel(haiku.alias);
    setHaiku1m(haiku.longContext);
    setKeyReplacedNotice(notice);
    setApplyResult(null);
  }, [pendingFillContent, keys, selectedKeyId, t]);

  const previewSettings = useMemo(() => {
    if (!selectedKey) return null;
    const existing = currentSettings ?? {};
    const existingEnv = (existing.env ?? {}) as Record<string, string>;
    // 镜像后端 apply_claude_settings：先剔除旧 AUTH_TOKEN，再按固定顺序重设，
    // 保证 env key 顺序与后端写入完全一致（否则 JSON diff 误判"一样却显示改了"）。
    const baseEnv: Record<string, string> = {};
    for (const [k, v] of Object.entries(existingEnv)) {
      if (k !== "ANTHROPIC_AUTH_TOKEN") baseEnv[k] = v;
    }
    baseEnv.ANTHROPIC_BASE_URL = `${gatewayUrl}/v1`;
    baseEnv.ANTHROPIC_AUTH_TOKEN = selectedKey.key;
    const opusVal = withLongContext(opusModel, opus1m);
    const sonnetVal = withLongContext(sonnetModel, sonnet1m);
    const haikuVal = withLongContext(haikuModel, haiku1m);
    if (opusVal) baseEnv.ANTHROPIC_DEFAULT_OPUS_MODEL = opusVal;
    else delete baseEnv.ANTHROPIC_DEFAULT_OPUS_MODEL;
    if (sonnetVal) baseEnv.ANTHROPIC_DEFAULT_SONNET_MODEL = sonnetVal;
    else delete baseEnv.ANTHROPIC_DEFAULT_SONNET_MODEL;
    if (haikuVal) baseEnv.ANTHROPIC_DEFAULT_HAIKU_MODEL = haikuVal;
    else delete baseEnv.ANTHROPIC_DEFAULT_HAIKU_MODEL;
    return { ...existing, env: baseEnv };
  }, [selectedKey, currentSettings, gatewayUrl, opusModel, sonnetModel, haikuModel, opus1m, sonnet1m, haiku1m]);

  const handleApply = async () => {
    if (!selectedKey) return;
    setApplying(true);
    setApplyResult(null);
    try {
      const data = await systemApi.applyClaudeSettings({
        apiBaseUrl: `${gatewayUrl}/v1`,
        apiKey: selectedKey.key,
        opusModel: withLongContext(opusModel, opus1m) || undefined,
        sonnetModel: withLongContext(sonnetModel, sonnet1m) || undefined,
        haikuModel: withLongContext(haikuModel, haiku1m) || undefined,
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
      isDirty(currentSettings, opusModel, sonnetModel, haikuModel, opus1m, sonnet1m, haiku1m),
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
        {/* 左列 */}
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
                label="Opus"
                envKey="ANTHROPIC_DEFAULT_OPUS_MODEL"
                models={allowedModelsList}
                value={opusModel}
                longContext={opus1m}
                onChange={setOpusModel}
                onLongContextChange={setOpus1m}
              />
              <ModelRoleSelect
                label="Sonnet"
                envKey="ANTHROPIC_DEFAULT_SONNET_MODEL"
                models={allowedModelsList}
                value={sonnetModel}
                longContext={sonnet1m}
                onChange={setSonnetModel}
                onLongContextChange={setSonnet1m}
              />
              <ModelRoleSelect
                label="Haiku"
                envKey="ANTHROPIC_DEFAULT_HAIKU_MODEL"
                models={allowedModelsList}
                value={haikuModel}
                longContext={haiku1m}
                onChange={setHaikuModel}
                onLongContextChange={setHaiku1m}
              />
              <p className="text-xs leading-relaxed text-muted-foreground">
                {t("setup.modelRolesHint")}
              </p>
            </div>
          )}

          <div className="rounded-lg border border-primary/10 bg-primary/5 p-3">
            <p className="text-xs leading-relaxed text-primary/80">
              {t("setup.endpointHint", { url: `${gatewayUrl}/v1` })}
            </p>
          </div>
          <ApplyButton
            selectedKey={!!selectedKey}
            applying={applying}
            settingsExists={settingsExists}
            applyResult={applyResult}
            onApply={handleApply}
            applyLabel={t("setup.claudeApplyBtn")}
            initLabel={t("setup.claudeInitBtn")}
          />
        </div>

        {/* 右列 */}
        <div className="space-y-3">
          <SettingsPreview
            settings={currentSettings}
            preview={previewSettings}
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
