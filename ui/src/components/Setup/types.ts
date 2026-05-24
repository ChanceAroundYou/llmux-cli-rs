import { Terminal, Code2, Wrench, Sparkles } from "lucide-react";

export interface ToolDef {
  id: string;
  detectKey: "claude" | "vscode" | "gemini" | "opencode" | "codex";
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
  {
    id: "gemini",
    detectKey: "gemini",
    label: "Gemini CLI",
    description: "Google Gemini CLI",
    icon: Sparkles,
    installUrl: "https://github.com/google-gemini/gemini-cli",
  },
  {
    id: "opencode",
    detectKey: "opencode",
    label: "OpenCode",
    description: "即将支持",
    icon: Wrench,
    installUrl: "https://github.com/opencode-ai/opencode",
    comingSoon: true,
  },
  {
    id: "codex",
    detectKey: "codex",
    label: "Codex",
    description: "OpenAI Codex CLI",
    icon: Terminal,
    installUrl: "https://github.com/openai/codex",
  },
];
