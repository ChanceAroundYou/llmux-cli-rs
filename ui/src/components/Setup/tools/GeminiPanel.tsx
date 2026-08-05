import { useState, useEffect, useMemo, useRef } from 'react';
import { useTranslation } from 'react-i18next';
import { Globe } from 'lucide-react';
import { KeySelector } from '../KeySelector';
import { ModelRoleSelect } from '../ModelRoleSelect';
import { CopyButton } from '../../CopyButton';
import { ApplyButton } from './ApplyButton';
import { KeyReplacedNotice } from './KeyReplacedNotice';
import { BackupHistory } from './BackupHistory';
import { GeminiSettingsPreview } from '../GeminiSettingsPreview';
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

function extractFromSettings(settingsStr: string): string {
  if (!settingsStr) return '';
  try {
    const parsed = JSON.parse(settingsStr);
    return parsed.model?.name ?? '';
  } catch {
    return '';
  }
}

function isDirty(
  currentSettings: Record<string, any> | null,
  model: string,
) {
  if (!currentSettings) return false;
  const settingsStr = currentSettings.settings ?? '';
  const cur = extractFromSettings(settingsStr);
  return cur !== model;
}

function getInitialModel(currentSettings: Record<string, any> | null): string {
  if (!currentSettings) return '';
  return extractFromSettings((currentSettings.settings as string) ?? '');
}

export function GeminiPanel({
  keys, aliases, gatewayUrl,
  currentSettings, settingsExists, settingsLoading, settingsFetched,
  onRefreshSettings, onSettingsApplied,
}: Props) {
  const { t } = useTranslation();
  const skipKeyClear = useRef(1);

  const [selectedKeyId, setSelectedKeyId] = useState<number | ''>(() => {
    if (!currentSettings || keys.length === 0) return '';
    return keys[0]?.id ?? '';
  });
  const [model, setModel] = useState(() => getInitialModel(currentSettings));

  const [applying, setApplying] = useState(false);
  const [applyResult, setApplyResult] = useState<{ success: boolean; backupPath?: string; error?: string } | null>(null);
  const [keyReplacedNotice, setKeyReplacedNotice] = useState<string | null>(null);

  const [backups, setBackups] = useState<BackupEntry[]>([]);
  const [backupsLoading, setBackupsLoading] = useState(false);
  const [expandedBackup, setExpandedBackup] = useState<string | null>(null);
  const [backupContents, setBackupContents] = useState<Record<string, Record<string, any>>>({});
  const [pendingFillContent, setPendingFillContent] = useState<Record<string, any> | null>(null);
  const [isRestoring, setIsRestoring] = useState(false);

  const [dirtyModalOpen, setDirtyModalOpen] = useState(false);
  const [pendingRestoreName, setPendingRestoreName] = useState<string | null>(null);
  const [deleteModalName, setDeleteModalName] = useState<string | null>(null);

  const selectedKey = keys.find(k => k.id === selectedKeyId);
  const allowedModelsList = selectedKey
    ? selectedKey.allowed_models === '*'
      ? aliases.map(a => a.alias)
      : parseAllowedModels(selectedKey.allowed_models)
    : [];

  useEffect(() => {
    if (skipKeyClear.current > 0) { skipKeyClear.current--; return; }
    setModel('');
    setApplyResult(null);
    setKeyReplacedNotice(null);
  }, [selectedKeyId]);

  const fetchBackups = async () => {
    setBackupsLoading(true);
    try {
      const res = await fetch('/api/system/gemini-backups');
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

    const settingsContent = content.settings ?? '';
    skipKeyClear.current += 1;
    let notice: string | null = null;
    // 备份 key 仍在本机密钥列表时优先选中它；否则回退到当前选中/第一个 key 并提示替换。
    const backupKeyMatch = (settingsContent as string).match(/^GEMINI_API_KEY=(\S+)$/m);
    const backupApiKey = backupKeyMatch?.[1] ?? '';
    const matchedKey = backupApiKey ? keys.find(k => k.key === backupApiKey) : undefined;
    const fallback = keys.find(k => k.id === selectedKeyId) ?? keys[0];
    if (matchedKey) {
      setSelectedKeyId(matchedKey.id);
    } else if (fallback) {
      setSelectedKeyId(fallback.id);
      notice = `备份中的 API Key 不在当前密钥列表，已自动替换为「${fallback.name}」。`;
    }
    const extracted = extractFromSettings(settingsContent as string);
    if (extracted) setModel(extracted);
    setKeyReplacedNotice(notice);
    setApplyResult(null);
  }, [pendingFillContent, keys]);

  const previewEnv = useMemo(() => {
    if (!selectedKey) return null;
    let result = (currentSettings?.env as string) ?? '';
    result = result.replace(/^GEMINI_API_KEY=.*$/m, `GEMINI_API_KEY=${selectedKey.key}`);
    if (!/^GEMINI_API_KEY=/m.test(result)) {
      result = result.trimEnd() + `\nGEMINI_API_KEY=${selectedKey.key}`;
    }
    result = result.replace(/^GOOGLE_GEMINI_BASE_URL=.*$/m, `GOOGLE_GEMINI_BASE_URL=${gatewayUrl}`);
    if (!/^GOOGLE_GEMINI_BASE_URL=/m.test(result)) {
      result = result.trimEnd() + `\nGOOGLE_GEMINI_BASE_URL=${gatewayUrl}`;
    }
    return result.trim();
  }, [selectedKey, currentSettings, gatewayUrl]);

  const previewSettings = useMemo(() => {
    if (!model) return null;
    const existingStr = (currentSettings?.settings as string) ?? '';
    let parsed: Record<string, any> = {};
    try {
      parsed = existingStr ? JSON.parse(existingStr) : {};
    } catch { /* ignore */ }
    parsed.model = { ...(parsed.model ?? {}), name: model };
    return JSON.stringify(parsed, null, 2);
  }, [model, currentSettings]);

  const handleApply = async () => {
    if (!selectedKey) return;
    setApplying(true);
    setApplyResult(null);
    try {
      const res = await fetch('/api/system/gemini-settings', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          apiKey: selectedKey.key,
          gatewayUrl,
          model: model || undefined,
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
      const res = await fetch(`/api/system/gemini-backups?name=${encodeURIComponent(name)}`);
      const data = await res.json();
      if (!data.settings) { setIsRestoring(false); return; }
      content = data.settings;
      setBackupContents(prev => ({ ...prev, [name]: content }));
    }
    const dirty = isDirty(currentSettings, model);
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
      const res = await fetch(`/api/system/gemini-backups?name=${encodeURIComponent(name)}`);
      const data = await res.json();
      if (data.settings) {
        setBackupContents(prev => ({ ...prev, [name]: data.settings }));
      }
    }
  };

  const handleDeleteConfirm = async () => {
    if (!deleteModalName) return;
    await fetch('/api/system/gemini-backups', {
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
                envKey="gemini_model"
                models={allowedModelsList}
                value={model}
                onChange={setModel}
              />
              <p className="text-xs text-muted-foreground leading-relaxed">{t('setup.geminiModelHint')}</p>
            </div>
          )}

          <div className="p-3 bg-primary/5 border border-primary/10 rounded-lg">
            <p className="text-xs text-primary/80 leading-relaxed">{t('setup.geminiEndpointHint')}</p>
          </div>
          <ApplyButton
            selectedKey={!!selectedKey}
            applying={applying}
            settingsExists={settingsExists}
            applyResult={applyResult}
            onApply={handleApply}
            applyLabel={t('setup.geminiApplyBtn')}
            initLabel={t('setup.geminiInitBtn')}
          />
        </div>

        <div className="space-y-3">
          <GeminiSettingsPreview
            currentEnv={currentSettings?.env ?? null}
            previewEnv={previewEnv}
            currentSettings={currentSettings?.settings ?? null}
            previewSettings={previewSettings}
            exists={settingsExists}
            loading={settingsLoading}
            onRefresh={onRefreshSettings}
          />
          <div className="flex items-center gap-3 px-4 py-2.5 rounded-xl border border-border bg-card text-xs text-muted-foreground">
            <Globe size={13} className="shrink-0 text-primary" />
            <span>{t('setup.gatewayUrl')} <span className="font-mono text-foreground">{gatewayUrl}</span></span>
            <CopyButton value={`${gatewayUrl}`} size={12} className="ml-auto" />
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
