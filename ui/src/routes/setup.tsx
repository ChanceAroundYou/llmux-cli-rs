import React, { useState, useEffect, useCallback } from 'react';
import { useTranslation } from 'react-i18next';
import { Wrench, RotateCcw } from 'lucide-react';
import { useKeysStore } from '../stores/keys';
import { useModelsStore } from '../stores/models';
import { TOOLS } from '../components/Setup/types';
import { ToolSidebar } from '../components/Setup/ToolSidebar';
import { ToolHeader } from '../components/Setup/ToolHeader';
import { ClaudeCodePanel } from '../components/Setup/tools/ClaudeCodePanel';

export default function Setup() {
  const { t } = useTranslation();
  const { keys, fetchKeys } = useKeysStore();
  const { aliases, fetchAliases } = useModelsStore();

  const [selectedTool, setSelectedTool] = useState('claude-code');
  const [installed, setInstalled] = useState<Record<string, boolean>>({});
  const [detectLoaded, setDetectLoaded] = useState(false);

  const [currentSettings, setCurrentSettings] = useState<Record<string, any> | null>(null);
  const [settingsExists, setSettingsExists] = useState(false);
  const [settingsLoading, setSettingsLoading] = useState(false);
  const [settingsFetched, setSettingsFetched] = useState(false);
  const [keysFetched, setKeysFetched] = useState(false);

  const fetchClaudeSettings = useCallback(async () => {
    setSettingsLoading(true);
    try {
      const res = await fetch('/api/system/claude-settings');
      const data = await res.json();
      setSettingsExists(data.exists);
      setCurrentSettings(data.settings);
    } finally {
      setSettingsLoading(false);
      setSettingsFetched(true);
    }
  }, []);

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
    <div className="flex gap-0 h-full min-h-[calc(100vh-8rem)] animate-in fade-in duration-500">
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

        {selectedTool === 'claude-code' && (
          !settingsFetched || !keysFetched ? (
            <div className="flex items-center gap-2 py-8 text-sm text-muted-foreground">
              <RotateCcw size={14} className="animate-spin shrink-0" />
              <span>{t('setup.loading')}</span>
            </div>
          ) : (
            <ClaudeCodePanel
              keys={keys}
              aliases={aliases}
              gatewayUrl={gatewayUrl}
              currentSettings={currentSettings}
              settingsExists={settingsExists}
              settingsLoading={settingsLoading}
              settingsFetched={settingsFetched}
              onRefreshSettings={fetchClaudeSettings}
              onSettingsApplied={(s) => { setCurrentSettings(s); setSettingsExists(true); }}
            />
          )
        )}
      </div>
    </div>
  );
}
