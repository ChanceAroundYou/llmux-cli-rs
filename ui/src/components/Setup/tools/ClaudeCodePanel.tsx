import { useState, useEffect, useMemo, useRef } from 'react';
import { useTranslation } from 'react-i18next';
import { Globe } from 'lucide-react';
import { KeySelector } from '../KeySelector';
import { ModelRoleSelect } from '../ModelRoleSelect';
import { SettingsPreview } from '../SettingsPreview';
import { CopyButton } from '../../CopyButton';
import { ApplyButton } from './ApplyButton';
import { KeyReplacedNotice } from './KeyReplacedNotice';
import { BackupHistory } from './BackupHistory';
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

function withLongContext(alias: string, enabled: boolean) {
  return alias && enabled ? `${alias}[1m]` : alias;
}

function isDirty(
  currentSettings: Record<string, any> | null,
  opusModel: string, sonnetModel: string, haikuModel: string,
  opus1m: boolean, sonnet1m: boolean, haiku1m: boolean,
) {
  if (!currentSettings) return false;
  const env = currentSettings.env ?? {};
  if ((env.ANTHROPIC_DEFAULT_OPUS_MODEL   ?? '') !== withLongContext(opusModel, opus1m))   return true;
  if ((env.ANTHROPIC_DEFAULT_SONNET_MODEL ?? '') !== withLongContext(sonnetModel, sonnet1m)) return true;
  if ((env.ANTHROPIC_DEFAULT_HAIKU_MODEL  ?? '') !== withLongContext(haikuModel, haiku1m))  return true;
  return false;
}

function parseModel(val?: string) {
  if (!val) return { alias: '', longContext: false };
  if (val.endsWith('[1m]')) return { alias: val.slice(0, -4), longContext: true };
  return { alias: val, longContext: false };
}

export function ClaudeCodePanel({
  keys, aliases, gatewayUrl,
  currentSettings, settingsExists, settingsLoading, settingsFetched,
  onRefreshSettings, onSettingsApplied,
}: Props) {
  const { t } = useTranslation();
  const initializedFromSettings = useRef(false);
  const restoringRef = useRef(false);
  const skipNextKeyCleanup = useRef(0);

  const [selectedKeyId, setSelectedKeyId] = useState<number | ''>('');
  const [opusModel, setOpusModel]     = useState('');
  const [sonnetModel, setSonnetModel] = useState('');
  const [haikuModel, setHaikuModel]   = useState('');
  const [opus1m, setOpus1m]     = useState(false);
  const [sonnet1m, setSonnet1m] = useState(false);
  const [haiku1m, setHaiku1m]   = useState(false);

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

  // 提前计算，effect 里需要用
  const selectedKey = keys.find(k => k.id === selectedKeyId);
  const allowedModelsList = selectedKey
    ? selectedKey.allowed_models === '*'
      ? aliases.map(a => a.alias)
      : parseAllowedModels(selectedKey.allowed_models)
    : [];

  // Modal 状态
  const [dirtyModalOpen, setDirtyModalOpen] = useState(false);
  const [pendingRestoreName, setPendingRestoreName] = useState<string | null>(null);
  const [deleteModalName, setDeleteModalName] = useState<string | null>(null);

  // 初始化：settings 和 keys 都就绪后，一次性回填 key + 模型
  useEffect(() => {
    if (!settingsFetched || initializedFromSettings.current || keys.length === 0) return;
    initializedFromSettings.current = true;

    const env = currentSettings?.env ?? {};
    const backupApiKey = env.ANTHROPIC_API_KEY ?? env.ANTHROPIC_AUTH_TOKEN ?? '';
    const matchedKey = keys.find(k => k.key === backupApiKey);

    skipNextKeyCleanup.current = 2;
    if (matchedKey) setSelectedKeyId(matchedKey.id);
    else setSelectedKeyId(keys[0].id);

    // 模型回填：直接在这里做，不依赖 allowedModelsList（避免 ref 时序问题）
    const opus   = parseModel(env.ANTHROPIC_DEFAULT_OPUS_MODEL);
    const sonnet = parseModel(env.ANTHROPIC_DEFAULT_SONNET_MODEL);
    const haiku  = parseModel(env.ANTHROPIC_DEFAULT_HAIKU_MODEL);
    if (opus.alias)   { setOpusModel(opus.alias);     setOpus1m(opus.longContext); }
    if (sonnet.alias) { setSonnetModel(sonnet.alias); setSonnet1m(sonnet.longContext); }
    if (haiku.alias)  { setHaikuModel(haiku.alias);   setHaiku1m(haiku.longContext); }
  }, [settingsFetched, keys]);

  // key 切换时清空模型（跳过初始化和还原）
  useEffect(() => {
    if (!initializedFromSettings.current) return;
    if (skipNextKeyCleanup.current > 0) { skipNextKeyCleanup.current--; return; }
    if (restoringRef.current) { restoringRef.current = false; return; }
    setOpusModel(''); setSonnetModel(''); setHaikuModel('');
    setOpus1m(false); setSonnet1m(false); setHaiku1m(false);
    setApplyResult(null);
    setKeyReplacedNotice(null);
  }, [selectedKeyId]);

  // 首次 keys 加载且没有 settings 时，选第一个
  useEffect(() => {
    if (keys.length > 0 && selectedKeyId === '' && !currentSettings) {
      setSelectedKeyId(keys[0].id);
    }
  }, [keys]);

  const fetchBackups = async () => {
    setBackupsLoading(true);
    try {
      const res = await fetch('/api/system/claude-backups');
      setBackups(await res.json());
    } finally {
      setBackupsLoading(false);
    }
  };

  useEffect(() => { fetchBackups(); }, []);

  // 消费 pendingFillContent，确保每次用最新的 keys/setters
  useEffect(() => {
    if (!pendingFillContent || keys.length === 0) return;
    const content = pendingFillContent;
    setPendingFillContent(null);
    setIsRestoring(false);

    const env = content.env ?? {};
    const backupApiKey = env.ANTHROPIC_API_KEY ?? env.ANTHROPIC_AUTH_TOKEN ?? '';
    const matchedKey = keys.find(k => k.key === backupApiKey);
    restoringRef.current = true;
    let notice: string | null = null;
    if (matchedKey) {
      setSelectedKeyId(matchedKey.id);
    } else {
      const fallback = keys.find(k => k.id === selectedKeyId) ?? keys[0];
      if (fallback) {
        setSelectedKeyId(fallback.id);
        notice = `备份中的 API Key (${backupApiKey.slice(0, 12)}••••) 不在当前密钥列表，已自动替换为「${fallback.name}」。`;
      }
    }
    const opus   = parseModel(env.ANTHROPIC_DEFAULT_OPUS_MODEL);
    const sonnet = parseModel(env.ANTHROPIC_DEFAULT_SONNET_MODEL);
    const haiku  = parseModel(env.ANTHROPIC_DEFAULT_HAIKU_MODEL);
    setOpusModel(opus.alias);     setOpus1m(opus.longContext);
    setSonnetModel(sonnet.alias); setSonnet1m(sonnet.longContext);
    setHaikuModel(haiku.alias);   setHaiku1m(haiku.longContext);
    setKeyReplacedNotice(notice);
    setApplyResult(null);
  }, [pendingFillContent, keys]);

  const previewSettings = useMemo(() => {
    if (!selectedKey) return null;
    const existing = currentSettings ?? {};
    const baseEnv = { ...(existing.env ?? {}) };
    delete baseEnv.ANTHROPIC_AUTH_TOKEN;
    const newEnv: Record<string, string> = {
      ...baseEnv,
      ANTHROPIC_BASE_URL: `${gatewayUrl}/v1`,
      ANTHROPIC_API_KEY: selectedKey.key,
    };
    const opusVal   = withLongContext(opusModel, opus1m);
    const sonnetVal = withLongContext(sonnetModel, sonnet1m);
    const haikuVal  = withLongContext(haikuModel, haiku1m);
    if (opusVal)   newEnv.ANTHROPIC_DEFAULT_OPUS_MODEL   = opusVal;
    else           delete newEnv.ANTHROPIC_DEFAULT_OPUS_MODEL;
    if (sonnetVal) newEnv.ANTHROPIC_DEFAULT_SONNET_MODEL = sonnetVal;
    else           delete newEnv.ANTHROPIC_DEFAULT_SONNET_MODEL;
    if (haikuVal)  newEnv.ANTHROPIC_DEFAULT_HAIKU_MODEL  = haikuVal;
    else           delete newEnv.ANTHROPIC_DEFAULT_HAIKU_MODEL;
    return { ...existing, env: newEnv };
  }, [selectedKey, currentSettings, gatewayUrl, opusModel, sonnetModel, haikuModel, opus1m, sonnet1m, haiku1m]);

  const handleApply = async () => {
    if (!selectedKey) return;
    setApplying(true);
    setApplyResult(null);
    try {
      const res = await fetch('/api/system/claude-settings', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          apiBaseUrl: `${gatewayUrl}/v1`,
          apiKey: selectedKey.key,
          opusModel:   withLongContext(opusModel, opus1m)     || undefined,
          sonnetModel: withLongContext(sonnetModel, sonnet1m) || undefined,
          haikuModel:  withLongContext(haikuModel, haiku1m)   || undefined,
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
      const res = await fetch(`/api/system/claude-backups?name=${encodeURIComponent(name)}`);
      const data = await res.json();
      if (!data.settings) { setIsRestoring(false); return; }
      content = data.settings;
      setBackupContents(prev => ({ ...prev, [name]: content }));
    }
    const dirty = isDirty(currentSettings, opusModel, sonnetModel, haikuModel, opus1m, sonnet1m, haiku1m);
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
      const res = await fetch(`/api/system/claude-backups?name=${encodeURIComponent(name)}`);
      const data = await res.json();
      if (data.settings) setBackupContents(prev => ({ ...prev, [name]: data.settings }));
    }
  };

  const handleDeleteConfirm = async () => {
    if (!deleteModalName) return;
    await fetch('/api/system/claude-backups', {
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
            <div className="text-xs font-bold text-muted-foreground uppercase tracking-widest">{t('setup.step1')}</div>
            <KeySelector keys={keys} selectedKeyId={selectedKeyId} onSelect={setSelectedKeyId} />
          </div>

          {allowedModelsList.length > 0 && (
            <div className="space-y-3 p-4 rounded-xl border border-border bg-card">
              <div className="text-xs font-bold text-muted-foreground uppercase tracking-widest">{t('setup.modelRoles')}</div>
              <ModelRoleSelect label="Opus"   envKey="ANTHROPIC_DEFAULT_OPUS_MODEL"   models={allowedModelsList} value={opusModel}   longContext={opus1m}   onChange={setOpusModel}   onLongContextChange={setOpus1m} />
              <ModelRoleSelect label="Sonnet" envKey="ANTHROPIC_DEFAULT_SONNET_MODEL" models={allowedModelsList} value={sonnetModel} longContext={sonnet1m} onChange={setSonnetModel} onLongContextChange={setSonnet1m} />
              <ModelRoleSelect label="Haiku"  envKey="ANTHROPIC_DEFAULT_HAIKU_MODEL"  models={allowedModelsList} value={haikuModel}  longContext={haiku1m}  onChange={setHaikuModel}  onLongContextChange={setHaiku1m} />
              <p className="text-[10px] text-muted-foreground leading-relaxed">{t('setup.modelRolesHint')}</p>
            </div>
          )}

          <div className="p-3 bg-blue-500/5 border border-blue-500/10 rounded-lg">
            <p className="text-[10px] text-blue-600/80 leading-relaxed">{t('setup.endpointHint')}</p>
          </div>
          <ApplyButton
            selectedKey={!!selectedKey}
            applying={applying}
            settingsExists={settingsExists}
            applyResult={applyResult}
            onApply={handleApply}
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
