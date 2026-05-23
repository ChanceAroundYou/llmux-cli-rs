import { Terminal, Code2, Wrench } from "lucide-react";

export interface ToolDef {
  id: string;
  detectKey: "claude" | "vscode" | "gemini" | "opencode";
  label: string;
  description: string;
  icon: React.ElementType;
  installUrl: string;
  comingSoon?: boolean;
}

export const TOOLS: ToolDef[] = [
  {
    id: "claude-code",
    detectKey: "claude",
    label: "Claude Code",
    description: "Anthropic 官方 CLI",
    icon: Terminal,
    installUrl: "https://docs.claude.com/en/docs/claude-code/quickstart",
  },
  {
    id: "vscode",
    detectKey: "vscode",
    label: "VSCode",
    description: "即将支持",
    icon: Code2,
    installUrl: "https://code.visualstudio.com/",
    comingSoon: true,
  },
  // {
  //   id: "gemini",
  //   detectKey: "gemini",
  //   label: "Gemini CLI",
  //   description: "即将支持",
  //   icon: Sparkles,
  //   installUrl: "https://github.com/google-gemini/gemini-cli",
  //   comingSoon: true,
  // },
  {
    id: "opencode",
    detectKey: "opencode",
    label: "OpenCode",
    description: "即将支持",
    icon: Wrench,
    installUrl: "https://github.com/opencode-ai/opencode",
    comingSoon: true,
  },
];
