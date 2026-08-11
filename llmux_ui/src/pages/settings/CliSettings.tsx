// 快速配置：CLI 工具一键接入网关（工具检测 + 密钥选择 + diff 预览 + 备份历史）
import React, { useCallback, useEffect, useState } from "react";
import { RotateCcw, Wrench } from "lucide-react";
import { useT } from "@/i18n";
import { useToast } from "@/hooks/useToast";
import { systemApi } from "@/api/system";
import { keyApi } from "@/api/keys";
import { modelApi } from "@/api/models";
import { PageHeader } from "@/components/shared/PageHeader";
import { ToolSidebar } from "@/components/setup/ToolSidebar";
import { ToolHeader } from "@/components/setup/ToolHeader";
import { ClaudeCodePanel } from "@/components/setup/panels/ClaudeCodePanel";
import { CodexPanel } from "@/components/setup/panels/CodexPanel";
import { GeminiPanel } from "@/components/setup/panels/GeminiPanel";
import { VSCodePanel } from "@/components/setup/panels/VSCodePanel";
import { TOOLS } from "@/components/setup/types";
import type { ApiKey } from "@/types/key";
import type { AliasResponse } from "@/types/model";

interface ClaudeData {
  exists: boolean;
  settings: Record<string, unknown> | null;
}
interface CodexData {
  exists: boolean;
  auth: Record<string, unknown> | null;
  configToml: string | null;
}
interface GeminiData {
  exists: boolean;
  env: string | null;
  settings: string | null;
}

export const CliSettings: React.FC = () => {
  const { t } = useT();
  const toast = useToast();

  const [selectedTool, setSelectedTool] = useState("claude-code");
  const [installed, setInstalled] = useState<Record<string, boolean>>({});
  const [detectLoaded, setDetectLoaded] = useState(false);
  const [keys, setKeys] = useState<ApiKey[]>([]);
  const [aliases, setAliases] = useState<AliasResponse[]>([]);
  const [loaded, setLoaded] = useState(false); // 首载完成（keys/aliases/各 settings）

  const [claudeData, setClaudeData] = useState<ClaudeData>({
    exists: false,
    settings: null,
  });
  const [codexData, setCodexData] = useState<CodexData>({
    exists: false,
    auth: null,
    configToml: null,
  });
  const [geminiData, setGeminiData] = useState<GeminiData>({
    exists: false,
    env: null,
    settings: null,
  });
  const [settingsLoading, setSettingsLoading] = useState(false);

  const fetchClaude = useCallback(async () => {
    setSettingsLoading(true);
    try {
      const d = await systemApi.getClaudeSettings();
      setClaudeData({
        exists: d.exists,
        settings: (d.settings as Record<string, unknown> | null) ?? null,
      });
    } finally {
      setSettingsLoading(false);
    }
  }, []);

  const fetchCodex = useCallback(async () => {
    setSettingsLoading(true);
    try {
      const d = await systemApi.getCodexSettings();
      setCodexData({ exists: d.exists, auth: d.auth, configToml: d.configToml });
    } finally {
      setSettingsLoading(false);
    }
  }, []);

  const fetchGemini = useCallback(async () => {
    setSettingsLoading(true);
    try {
      const d = await systemApi.getGeminiSettings();
      setGeminiData({ exists: d.exists, env: d.env, settings: d.settings });
    } finally {
      setSettingsLoading(false);
    }
  }, []);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      const [toolsRes, keysRes, aliasesRes] = await Promise.allSettled([
        systemApi.getTools(),
        keyApi.list(),
        modelApi.getAliases(),
      ]);
      if (!cancelled) {
        setInstalled(
          toolsRes.status === "fulfilled"
            ? (toolsRes.value as unknown as Record<string, boolean>)
            : {},
        );
        setDetectLoaded(true);
        if (keysRes.status === "fulfilled") setKeys(keysRes.value);
        if (aliasesRes.status === "fulfilled") setAliases(aliasesRes.value);
      }
      const settingsRes = await Promise.allSettled([
        fetchClaude(),
        fetchCodex(),
        fetchGemini(),
      ]);
      if (settingsRes.some((r) => r.status === "rejected")) {
        toast.error(t("setup.loadFailed"));
      }
      if (!cancelled) setLoaded(true);
    })();
    return () => {
      cancelled = true;
    };
  }, [fetchClaude, fetchCodex, fetchGemini, toast, t]);

  const tool = TOOLS.find((tt) => tt.id === selectedTool) ?? TOOLS[0];
  const isInstalled = installed[tool.detectKey] === true;
  // 网关地址：UI 内嵌进后端后同源，origin 即网关地址
  const gatewayUrl = window.location.origin;

  const onSettingsApplied = (key: "claude" | "codex" | "gemini") => {
    return (settings: Record<string, unknown>) => {
      if (key === "claude") {
        setClaudeData({ exists: true, settings });
      } else if (key === "codex") {
        setCodexData({
          exists: true,
          auth: (settings.auth as Record<string, unknown>) ?? null,
          configToml:
            typeof settings.configToml === "string" ? settings.configToml : null,
        });
      } else {
        setGeminiData({
          exists: true,
          env: typeof settings.env === "string" ? settings.env : null,
          settings:
            typeof settings.settings === "string" ? settings.settings : null,
        });
      }
    };
  };

  return (
    <div className="animate-fade-in space-y-6">
      <PageHeader
        icon={Wrench}
        iconClass="bg-warning/25 text-warning-foreground"
        title={t("cli.title")}
        description={t("cli.desc")}
      />

      <div className="flex flex-col gap-0 min-h-[calc(100dvh-18rem)] xl:flex-row">
        <ToolSidebar
          selectedTool={selectedTool}
          installed={installed}
          detectLoaded={detectLoaded}
          onSelect={setSelectedTool}
        />

        <div className="min-w-0 flex-1 space-y-5 pt-4 xl:pl-6 xl:pt-0">
          <ToolHeader
            tool={tool}
            isInstalled={isInstalled}
            detectLoaded={detectLoaded}
          />

          {tool.comingSoon && (
            <div className="flex items-center gap-3 rounded-xl border border-dashed border-border bg-muted/20 p-5 text-sm text-muted-foreground">
              <Wrench size={16} className="shrink-0" />
              <span>
                {t("setup.comingSoon", { tool: t(tool.labelKey) })}
              </span>
            </div>
          )}

          {!loaded || !detectLoaded ? (
            <div className="flex items-center gap-2 py-8 text-sm text-muted-foreground">
              <RotateCcw
                size={14}
                className="shrink-0 animate-[spin_1s_linear_infinite_reverse]"
              />
              <span>{t("setup.loading")}</span>
            </div>
          ) : tool.id === "vscode" ? (
            <VSCodePanel aliases={aliases} gatewayUrl={gatewayUrl} />
          ) : tool.id === "claude-code" ? (
            <ClaudeCodePanel
              keys={keys}
              aliases={aliases}
              gatewayUrl={gatewayUrl}
              currentSettings={claudeData.settings}
              settingsExists={claudeData.exists}
              settingsLoading={settingsLoading}
              settingsFetched={loaded}
              onRefreshSettings={fetchClaude}
              onSettingsApplied={onSettingsApplied("claude")}
            />
          ) : tool.id === "codex" ? (
            <CodexPanel
              keys={keys}
              aliases={aliases}
              gatewayUrl={gatewayUrl}
              currentAuth={codexData.auth}
              currentToml={codexData.configToml}
              settingsExists={codexData.exists}
              settingsLoading={settingsLoading}
              onRefreshSettings={fetchCodex}
              onSettingsApplied={onSettingsApplied("codex")}
            />
          ) : tool.id === "gemini" ? (
            <GeminiPanel
              keys={keys}
              aliases={aliases}
              gatewayUrl={gatewayUrl}
              currentEnv={geminiData.env}
              currentSettings={geminiData.settings}
              settingsExists={geminiData.exists}
              settingsLoading={settingsLoading}
              onRefreshSettings={fetchGemini}
              onSettingsApplied={onSettingsApplied("gemini")}
            />
          ) : null}
        </div>
      </div>
    </div>
  );
};
