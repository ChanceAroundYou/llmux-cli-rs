// 快速配置：CLI 工具定义
import type { LucideIcon } from "lucide-react";
import { Terminal, Code2, Sparkles, Wrench } from "lucide-react";

export type DetectKey = "claude" | "vscode" | "gemini" | "opencode" | "codex";

export interface ToolDef {
  id: string;
  detectKey: DetectKey;
  /** i18n key：工具名 */
  labelKey: string;
  /** i18n key：工具描述 */
  descKey: string;
  icon: LucideIcon;
  installUrl: string;
  comingSoon?: boolean;
}

export const TOOLS: ToolDef[] = [
  {
    id: "claude-code",
    detectKey: "claude",
    labelKey: "setup.tool.claude",
    descKey: "setup.tool.claudeDesc",
    icon: Terminal,
    installUrl: "https://docs.claude.com/en/docs/claude-code/quickstart",
  },
  {
    id: "codex",
    detectKey: "codex",
    labelKey: "setup.tool.codex",
    descKey: "setup.tool.codexDesc",
    icon: Terminal,
    installUrl: "https://github.com/openai/codex",
  },
  {
    id: "gemini",
    detectKey: "gemini",
    labelKey: "setup.tool.gemini",
    descKey: "setup.tool.geminiDesc",
    icon: Sparkles,
    installUrl: "https://github.com/google-gemini/gemini-cli",
  },
  {
    id: "vscode",
    detectKey: "vscode",
    labelKey: "setup.tool.vscode",
    descKey: "setup.tool.vscodeDesc",
    icon: Code2,
    installUrl: "https://code.visualstudio.com/",
  },
  {
    id: "opencode",
    detectKey: "opencode",
    labelKey: "setup.tool.opencode",
    descKey: "setup.tool.comingSoonDesc",
    icon: Wrench,
    installUrl: "https://github.com/opencode-ai/opencode",
    comingSoon: true,
  },
];
