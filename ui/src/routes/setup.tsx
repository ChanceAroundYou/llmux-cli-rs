import React, { useState, useEffect, useCallback } from 'react';
import { useTranslation } from 'react-i18next';
import { Wrench, RotateCcw, MonitorSmartphone } from 'lucide-react';
import { PageHeader } from '../components/shared/PageHeader';
import { useKeysStore } from '../stores/keys';
import { useModelsStore } from '../stores/models';
import { TOOLS } from '../components/Setup/types';
import { ToolSidebar } from '../components/Setup/ToolSidebar';
import { ToolHeader } from '../components/Setup/ToolHeader';
import { ClaudeCodePanel } from '../components/Setup/tools/ClaudeCodePanel';
import { CodexPanel } from '../components/Setup/tools/CodexPanel';

export default function Setup() {
  const { t } = useTranslation();
  const { keys, fetchKeys } = useKeysStore();
  const { aliases, fetchAliases } = useModelsStore();

  const [selectedTool, setSelectedTool] = useState('claude-code');
  const [installed, setInstalled] = useState<Record<string, boolean>>({});
  const [detectLoaded, setDetectLoaded] = useState(false);

  const [claudeSettings, setClaudeSettings] = useState<Record<string, any> | null>(null);
  const [codexSettings, setCodexSettings] = useState<Record<string, any> | null>(null);
  const [settingsExists, setSettingsExists] = useState(false);
  const [settingsLoading, setSettingsLoading] = useState(false);
  const [settingsFetched, setSettingsFetched] = useState(false);
  const [keysFetched, setKeysFetched] = useState(false);

  const fetchClaudeSettings = useCallback(async () => {
    setSettingsLoading(true);
    setSettingsFetched(false);
    try {
      const res = await fetch('/api/system/claude-settings');
      const data = await res.json();
      setSettingsExists(data.exists);
      setClaudeSettings(data.settings);
    } finally {
      setSettingsLoading(false);
      setSettingsFetched(true);
    }
  }, []);

  const fetchCodexSettings = useCallback(async () => {
    setSettingsLoading(true);
    setSettingsFetched(false);
    try {
      const res = await fetch('/api/system/codex-settings');
      const data = await res.json();
      setSettingsExists(data.exists);
      setCodexSettings(data.auth || data.configToml ? { auth: data.auth, configToml: data.configToml } : null);
    } finally {
      setSettingsLoading(false);
      setSettingsFetched(true);
    }
  }, []);

  const currentSettings = selectedTool === 'codex' ? codexSettings : claudeSettings;
  const onRefreshSettings = selectedTool === 'codex' ? fetchCodexSettings : fetchClaudeSettings;

  // 当切换工具时重新加载对应的 settings
  useEffect(() => {
    if (selectedTool === 'codex') fetchCodexSettings();
    else { setSettingsFetched(true); }
  }, [selectedTool]);

  useEffect(() => {
    fetchKeys().then(() => setKeysFetched(true));
    fetchAliases();
    fetch('/api/system/tools')
      .then(r => r.json())
      .then(data => { setInstalled(data); setDetectLoaded(true); })
      .catch(() => setDetectLoaded(true));
    fetchClaudeSettings();
  }, []);

  const tool = TOOLS.find(t => t.id === selectedTool)!;
  const isToolInstalled = installed[tool.detectKey] === true;
  const gatewayUrl = window.location.origin;

  return (
    <div className="space-y-8 animate-fadeIn duration-500">
      <PageHeader
        icon={<MonitorSmartphone size={24} />}
        title={t('common.setup')}
        subtitle={t('setup.subtitle')}
      />
      <div className="flex gap-0 h-full min-h-[calc(100vh-16rem)]">
      <ToolSidebar
        selectedTool={selectedTool}
        installed={installed}
        detectLoaded={detectLoaded}
        onSelect={setSelectedTool}
      />

      <div className="flex-1 pl-6 min-w-0 space-y-5">
        <ToolHeader tool={tool} isInstalled={isToolInstalled} detectLoaded={detectLoaded} />

        {tool.comingSoon && (
          <div className="flex items-center gap-3 p-5 rounded-xl border border-dashed border-border bg-muted/20 text-sm text-muted-foreground">
            <Wrench size={16} className="shrink-0" />
            <span>{t('setup.comingSoon', { tool: tool.label })}</span>
          </div>
        )}

        {!settingsFetched || !keysFetched ? (
          <div className="flex items-center gap-2 py-8 text-sm text-muted-foreground">
            <RotateCcw size={14} className="animate-spin shrink-0" />
            <span>{t('setup.loading')}</span>
          </div>
        ) : selectedTool === 'claude-code' ? (
          <ClaudeCodePanel
            keys={keys}
            aliases={aliases}
            gatewayUrl={gatewayUrl}
            currentSettings={currentSettings}
            settingsExists={settingsExists}
            settingsLoading={settingsLoading}
            settingsFetched={settingsFetched}
            onRefreshSettings={onRefreshSettings}
            onSettingsApplied={(s) => { setClaudeSettings(s); setSettingsExists(true); }}
          />
        ) : selectedTool === 'codex' ? (
          <CodexPanel
            keys={keys}
            aliases={aliases}
            gatewayUrl={gatewayUrl}
            currentSettings={currentSettings}
            settingsExists={settingsExists}
            settingsLoading={settingsLoading}
            settingsFetched={settingsFetched}
            onRefreshSettings={onRefreshSettings}
            onSettingsApplied={(s) => { setCodexSettings(s); setSettingsExists(true); }}
          />
        ) : null}
      </div>
    </div>
    </div>
  );
}
