import { useState, useEffect, useMemo, useRef } from 'react';
import { useTranslation } from 'react-i18next';
import { Globe } from 'lucide-react';
import { KeySelector } from '../KeySelector';
import { ModelRoleSelect } from '../ModelRoleSelect';
import { CopyButton } from '../../CopyButton';
import { ApplyButton } from './ApplyButton';
import { KeyReplacedNotice } from './KeyReplacedNotice';
import { BackupHistory } from './BackupHistory';
import { CodexSettingsPreview } from '../CodexSettingsPreview';
import type { BackupEntry } from './BackupHistory';
import type { ApiKey } from '../../../stores/keys';
import type { ModelAlias } from '../../../stores/models';
import { parseAllowedModels } from '../utils';

interface Props {
  keys: ApiKey[];
  aliases: ModelAlias[];
  gatewayUrl: string;
  currentSettings: Record<string, any> | null;
  settingsExists: boolean;
  settingsLoading: boolean;
  settingsFetched: boolean;
  onRefreshSettings: () => void;
  onSettingsApplied: (settings: Record<string, any>) => void;
}

function extractFromToml(configToml: string) {
  const model = (configToml.match(/model\s*=\s*"([^"]+)"/) ?? [])[1] ?? '';
  const wireApi = (configToml.match(/wire_api\s*=\s*"([^"]+)"/) ?? [])[1] ?? '';
  const ctxMatch = configToml.match(/model_context_window\s*=\s*(\d+)/);
  const ctx = ctxMatch ? parseInt(ctxMatch[1], 10) : 0;
  const compactMatch = configToml.match(/model_auto_compact_token_limit\s*=\s*(\d+)/);
  const compact = compactMatch ? parseInt(compactMatch[1], 10) : 0;
  return { model, wireApi, contextWindow: ctx, autoCompactLimit: compact };
}

function setTomlKey(toml: string, key: string, value: string): string {
  const re = new RegExp(`^${key}\\s*=\\s*"[^"]*"`, 'm');
  if (re.test(toml)) {
    return toml.replace(re, `${key} = "${value}"`);
  }
  return toml.trimEnd() + `\n${key} = "${value}"\n`;
}

function setTomlIntKey(toml: string, key: string, value: number): string {
  const re = new RegExp(`^${key}\\s*=\\s*\\d+`, 'm');
  if (re.test(toml)) {
    return toml.replace(re, `${key} = ${value}`);
  }
  return toml.trimEnd() + `\n${key} = ${value}\n`;
}

function patchToml(
  existing: string, model: string, wireApi: string, gatewayUrl: string,
  contextWindow: number, autoCompactLimit: number,
): string {
  let result = existing || '';

  result = setTomlKey(result, 'model_provider', 'llmux');
  result = setTomlKey(result, 'model', model || '(select a model)');
  result = setTomlKey(result, 'review_model', model || '(select a model)');
  if (contextWindow > 0) result = setTomlIntKey(result, 'model_context_window', contextWindow);
  if (autoCompactLimit > 0) result = setTomlIntKey(result, 'model_auto_compact_token_limit', autoCompactLimit);

  // 确保 provider section 存在，逐 key 更新，不重写整个 section
  const sectionHeader = '[model_providers.llmux]';
  if (!result.includes(sectionHeader)) {
    result = result.trimEnd() + `\n\n${sectionHeader}\n`;
  }
  result = setTomlKey(result, 'name', 'llmux');
  result = setTomlKey(result, 'base_url', `${gatewayUrl}/v1`);
  result = setTomlKey(result, 'wire_api', wireApi);
  const boolRe = /^requires_openai_auth\s*=\s*(?:true|false|"true"|"false")/m;
  if (boolRe.test(result)) {
    result = result.replace(boolRe, 'requires_openai_auth = true');
  }

  return result;
}

function isDirty(
  currentSettings: Record<string, any> | null,
  model: string,
  wireApi: string,
  contextWindow: number,
  autoCompactLimit: number,
) {
  if (!currentSettings) return false;
  const configToml = currentSettings.configToml ?? '';
  const cur = extractFromToml(configToml);
  return cur.model !== model || cur.wireApi !== wireApi
    || (contextWindow > 0 && cur.contextWindow !== contextWindow)
    || (autoCompactLimit > 0 && cur.autoCompactLimit !== autoCompactLimit);
}

function getInitialKeyId(
  currentSettings: Record<string, any> | null,
  keys: ApiKey[],
): number | '' {
  if (!currentSettings || keys.length === 0) return '';
  const backupApiKey = (currentSettings.auth as Record<string, any> | null)?.OPENAI_API_KEY ?? '';
  const matchedKey = keys.find(k => k.key === backupApiKey);
  return matchedKey ? matchedKey.id : (keys[0]?.id ?? '');
}

function getInitialModel(currentSettings: Record<string, any> | null): string {
  if (!currentSettings) return '';
  return extractFromToml((currentSettings.configToml as string) ?? '').model;
}

function getInitialWireApi(currentSettings: Record<string, any> | null): 'responses' | 'chat' {
  if (!currentSettings) return 'responses';
  return (extractFromToml((currentSettings.configToml as string) ?? '').wireApi as 'responses' | 'chat') || 'responses';
}

function getInitialNum(currentSettings: Record<string, any> | null, field: 'contextWindow' | 'autoCompactLimit'): number {
  if (!currentSettings) return 0;
  return extractFromToml((currentSettings.configToml as string) ?? '')[field];
}

export function CodexPanel({
  keys, aliases, gatewayUrl,
  currentSettings, settingsExists, settingsLoading, settingsFetched,
  onRefreshSettings, onSettingsApplied,
}: Props) {
  const { t } = useTranslation();
  const skipKeyClear = useRef(1); // skip first key-switch fire (mount)

  const [selectedKeyId, setSelectedKeyId] = useState<number | ''>(
    () => getInitialKeyId(currentSettings, keys),
  );
  const [model, setModel] = useState(() => getInitialModel(currentSettings));
  const [wireApi, setWireApi] = useState<'responses' | 'chat'>(
    () => getInitialWireApi(currentSettings),
  );
  const [contextWindow, setContextWindow] = useState(
    () => getInitialNum(currentSettings, 'contextWindow'),
  );
  const [autoCompactLimit, setAutoCompactLimit] = useState(
    () => getInitialNum(currentSettings, 'autoCompactLimit'),
  );

  const [applying, setApplying] = useState(false);
  const [applyResult, setApplyResult] = useState<{ success: boolean; backupPath?: string; error?: string } | null>(null);
  const [keyReplacedNotice, setKeyReplacedNotice] = useState<string | null>(null);

  // 备份相关
  const [backups, setBackups] = useState<BackupEntry[]>([]);
  const [backupsLoading, setBackupsLoading] = useState(false);
  const [expandedBackup, setExpandedBackup] = useState<string | null>(null);
  const [backupContents, setBackupContents] = useState<Record<string, Record<string, any>>>({});
  const [pendingFillContent, setPendingFillContent] = useState<Record<string, any> | null>(null);
  const [isRestoring, setIsRestoring] = useState(false);

  // Modal 状态
  const [dirtyModalOpen, setDirtyModalOpen] = useState(false);
  const [pendingRestoreName, setPendingRestoreName] = useState<string | null>(null);
  const [deleteModalName, setDeleteModalName] = useState<string | null>(null);

  const selectedKey = keys.find(k => k.id === selectedKeyId);
  const allowedModelsList = selectedKey
    ? selectedKey.allowed_models === '*'
      ? aliases.map(a => a.alias)
      : parseAllowedModels(selectedKey.allowed_models)
    : [];

  // key 切换时清空模型（mount 和 restore 时通过 skipKeyClear 跳过）
  useEffect(() => {
    if (skipKeyClear.current > 0) { skipKeyClear.current--; return; }
    setModel('');
    setWireApi('responses');
    setContextWindow(0);
    setAutoCompactLimit(0);
    setApplyResult(null);
    setKeyReplacedNotice(null);
  }, [selectedKeyId]);

  const fetchBackups = async () => {
    setBackupsLoading(true);
    try {
      const res = await fetch('/api/system/codex-backups');
      setBackups(await res.json());
    } finally {
      setBackupsLoading(false);
    }
  };

  useEffect(() => { fetchBackups(); }, []);

  useEffect(() => {
    if (!pendingFillContent || keys.length === 0) return;
    const content = pendingFillContent;
    setPendingFillContent(null);
    setIsRestoring(false);

    const auth = content.auth ?? {};
    const configToml = content.configToml ?? '';
    const backupApiKey = auth.OPENAI_API_KEY ?? '';
    const matchedKey = keys.find(k => k.key === backupApiKey);
    skipKeyClear.current += 1; // 跳过因 restore 触发的 key 切换清空
    let notice: string | null = null;
    if (matchedKey) {
      setSelectedKeyId(matchedKey.id);
    } else {
      const fallback = keys.find(k => k.id === selectedKeyId) ?? keys[0];
      if (fallback) {
        setSelectedKeyId(fallback.id);
        notice = `备份中的 API Key 不在当前密钥列表，已自动替换为「${fallback.name}」。`;
      }
    }
    const extracted = extractFromToml(configToml);
    if (extracted.model) setModel(extracted.model);
    if (extracted.wireApi) setWireApi(extracted.wireApi as 'responses' | 'chat');
    if (extracted.contextWindow) setContextWindow(extracted.contextWindow);
    if (extracted.autoCompactLimit) setAutoCompactLimit(extracted.autoCompactLimit);
    setKeyReplacedNotice(notice);
    setApplyResult(null);
  }, [pendingFillContent, keys]);

  const previewSettings = useMemo(() => {
    if (!selectedKey) return null;
    const existing = currentSettings ?? {};
    const newAuth = { OPENAI_API_KEY: selectedKey.key };
    const existingToml = (currentSettings?.configToml as string) ?? '';
    const newConfigToml = patchToml(existingToml, model, wireApi, gatewayUrl, contextWindow, autoCompactLimit);
    return { ...existing, auth: newAuth, configToml: newConfigToml };
  }, [selectedKey, currentSettings, gatewayUrl, model, wireApi, contextWindow, autoCompactLimit]);

  const handleApply = async () => {
    if (!selectedKey) return;
    setApplying(true);
    setApplyResult(null);
    try {
      const res = await fetch('/api/system/codex-settings', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          apiBaseUrl: `${gatewayUrl}/v1`,
          apiKey: selectedKey.key,
          model: model || undefined,
          reviewModel: model || undefined,
          wireApi,
          contextWindow: contextWindow || undefined,
          autoCompactLimit: autoCompactLimit || undefined,
        }),
      });
      const data = await res.json();
      if (data.success) {
        setApplyResult({ success: true, backupPath: data.backupPath });
        onSettingsApplied(data.settings);
        setKeyReplacedNotice(null);
        fetchBackups();
      } else {
        setApplyResult({ success: false, error: data.error });
      }
    } catch (err: any) {
      setApplyResult({ success: false, error: err.message });
    } finally {
      setApplying(false);
    }
  };

  const loadBackupIntoForm = (content: Record<string, any>) => {
    setPendingFillContent(content);
  };

  const handleRestoreClick = async (name: string) => {
    if (isRestoring) return;
    setIsRestoring(true);
    let content = backupContents[name];
    if (!content) {
      const res = await fetch(`/api/system/codex-backups?name=${encodeURIComponent(name)}`);
      const data = await res.json();
      if (!data.settings) { setIsRestoring(false); return; }
      content = data.settings;
      setBackupContents(prev => ({ ...prev, [name]: content }));
    }
    const dirty = isDirty(currentSettings, model, wireApi, contextWindow, autoCompactLimit);
    if (dirty) {
      setPendingRestoreName(name);
      setDirtyModalOpen(true);
      setIsRestoring(false);
    } else {
      loadBackupIntoForm(content);
    }
  };

  const handleToggleExpand = async (name: string) => {
    if (expandedBackup === name) { setExpandedBackup(null); return; }
    setExpandedBackup(name);
    if (!backupContents[name]) {
      const res = await fetch(`/api/system/codex-backups?name=${encodeURIComponent(name)}`);
      const data = await res.json();
      if (data.settings) {
        setBackupContents(prev => ({ ...prev, [name]: data.settings }));
      }
    }
  };

  const handleDeleteConfirm = async () => {
    if (!deleteModalName) return;
    await fetch('/api/system/codex-backups', {
      method: 'DELETE',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ name: deleteModalName }),
    });
    if (expandedBackup === deleteModalName) setExpandedBackup(null);
    setDeleteModalName(null);
    fetchBackups();
  };

  return (
    <div className="space-y-5">
      <KeyReplacedNotice notice={keyReplacedNotice} onDismiss={() => setKeyReplacedNotice(null)} />

      <div className="grid grid-cols-1 xl:grid-cols-2 gap-5">
        {/* 左列 */}
        <div className="space-y-4">
          <div className="space-y-2">
            <div className="text-xs font-semibold text-muted-foreground uppercase tracking-widest">{t('setup.step1')}</div>
            <KeySelector keys={keys} selectedKeyId={selectedKeyId} onSelect={setSelectedKeyId} />
          </div>

          {allowedModelsList.length > 0 && (
            <div className="space-y-3 p-4 rounded-xl border border-border bg-card">
              <div className="text-xs font-semibold text-muted-foreground uppercase tracking-widest">{t('setup.modelRoles')}</div>
              <ModelRoleSelect
                label={t('setup.defaultModel')}
                envKey="codex_model"
                models={allowedModelsList}
                value={model}
                onChange={setModel}
              />
              <div className="space-y-1.5">
                <label className="text-xs font-bold text-muted-foreground uppercase">{t('setup.wireApi')}</label>
                <div className="flex border border-border rounded-lg overflow-hidden text-xs font-semibold">
                  <button
                    type="button"
                    onClick={() => setWireApi('responses')}
                    className={`flex-1 px-3 py-1.5 transition-colors ${wireApi === 'responses' ? 'bg-primary text-primary-foreground' : 'hover:bg-muted'}`}
                  >
                    responses
                  </button>
                  <button
                    type="button"
                    onClick={() => setWireApi('chat')}
                    className={`flex-1 px-3 py-1.5 transition-colors ${wireApi === 'chat' ? 'bg-primary text-primary-foreground' : 'hover:bg-muted'}`}
                  >
                    chat
                  </button>
                </div>
                <p className="text-xs text-muted-foreground">{t('setup.wireApiHint')}</p>
              </div>
              <div className="grid grid-cols-2 gap-3">
                <div className="space-y-1">
                  <label className="text-xs font-semibold text-muted-foreground uppercase">{t('setup.contextWindow')}</label>
                  <input
                    type="number"
                    value={contextWindow || ''}
                    onChange={e => setContextWindow(parseInt(e.target.value, 10) || 0)}
                    placeholder="1000000"
                    className="w-full bg-card border border-border rounded-lg px-3 py-1.5 text-xs font-mono"
                  />
                  <p className="text-xs text-muted-foreground/70">{t('setup.contextWindowHint')}</p>
                </div>
                <div className="space-y-1">
                  <label className="text-xs font-semibold text-muted-foreground uppercase">{t('setup.autoCompactLimit')}</label>
                  <input
                    type="number"
                    value={autoCompactLimit || ''}
                    onChange={e => setAutoCompactLimit(parseInt(e.target.value, 10) || 0)}
                    placeholder="900000"
                    className="w-full bg-card border border-border rounded-lg px-3 py-1.5 text-xs font-mono"
                  />
                  <p className="text-xs text-muted-foreground/70">{t('setup.autoCompactLimitHint')}</p>
                </div>
              </div>
              <p className="text-xs text-muted-foreground leading-relaxed">{t('setup.codexModelRolesHint')}</p>
            </div>
          )}

          <div className="p-3 bg-primary/5 border border-primary/10 rounded-lg">
            <p className="text-xs text-primary/80 leading-relaxed">{t('setup.codexEndpointHint')}</p>
          </div>
          <ApplyButton
            selectedKey={!!selectedKey}
            applying={applying}
            settingsExists={settingsExists}
            applyResult={applyResult}
            onApply={handleApply}
            applyLabel={t('setup.codexApplyBtn')}
            initLabel={t('setup.codexInitBtn')}
          />
        </div>

        {/* 右列 */}
        <div className="space-y-3">
          <CodexSettingsPreview
            currentAuth={currentSettings?.auth ?? null}
            previewAuth={previewSettings?.auth ?? null}
            currentToml={currentSettings?.configToml ?? null}
            previewToml={previewSettings?.configToml ?? null}
            exists={settingsExists}
            loading={settingsLoading}
            onRefresh={onRefreshSettings}
          />
          <div className="flex items-center gap-3 px-4 py-2.5 rounded-xl border border-border bg-card text-xs text-muted-foreground">
            <Globe size={13} className="shrink-0 text-primary" />
            <span>{t('setup.gatewayUrl')} <span className="font-mono text-foreground">{gatewayUrl}/v1</span></span>
            <CopyButton value={`${gatewayUrl}/v1`} size={12} className="ml-auto" />
          </div>
        </div>
      </div>

      <BackupHistory
        backups={backups}
        backupsLoading={backupsLoading}
        isRestoring={isRestoring}
        pendingRestoreName={pendingRestoreName}
        expandedBackup={expandedBackup}
        backupContents={backupContents}
        dirtyModalOpen={dirtyModalOpen}
        deleteModalName={deleteModalName}
        onToggleExpand={handleToggleExpand}
        onRestoreClick={handleRestoreClick}
        onDeleteClick={setDeleteModalName}
        onDirtyModalClose={() => { setDirtyModalOpen(false); setPendingRestoreName(null); }}
        onDirtyModalConfirm={() => {
          setDirtyModalOpen(false);
          if (pendingRestoreName && backupContents[pendingRestoreName]) {
            loadBackupIntoForm(backupContents[pendingRestoreName]);
          }
          setPendingRestoreName(null);
        }}
        onDeleteModalClose={() => setDeleteModalName(null)}
        onDeleteConfirm={handleDeleteConfirm}
      />
    </div>
  );
}

