// 临时 diff 效果 demo：react-diff-viewer-continued 单栏 + 项目 token 颜色
import React from "react";
import { DiffViewer } from "@/components/shared/DiffViewer";

const oldContent = `# Codex 配置示例（修改前）
model = "gpt-4o"

[llmux]
base_url = "http://localhost:25975"
api_key = "sk-old-key"
env_key = "OPENAI_API_KEY"

[llmux.models]
model = "gpt-4o"
temperature = 0.7`;

const newContent = `# Codex 配置示例（修改后）
model = "claude-sonnet-5"

[llmux]
base_url = "http://localhost:25975"
api_key = "sk-llmux-new-key"
env_key = "ANTHROPIC_API_KEY"

[llmux.models]
model = "claude-sonnet-5"
temperature = 0.2
max_tokens = 4096`;

export const DiffDemo: React.FC = () => {
  return (
    <div className="animate-fade-in space-y-6 p-6">
      <div>
        <h1 className="text-lg font-semibold text-card-foreground">
          Diff 效果 Demo · react-diff-viewer-continued（单栏）
        </h1>
        <p className="mt-1 text-sm text-muted-foreground">
          库内置行级 + 词级高亮；颜色用项目 CSS 变量，明暗与色板自动跟随主题。
        </p>
      </div>

      <div className="space-y-2">
        <h2 className="text-xs font-semibold uppercase tracking-[0.2em] text-muted-foreground">
          单栏 Unified · 折叠未变区块 · 词级高亮
        </h2>
        <DiffViewer oldValue={oldContent} newValue={newContent} maxHeight="420px" />
      </div>

      <p className="text-xs text-muted-foreground">
        暗色模式：把应用切到暗色（右上角主题），颜色自动变暗。
      </p>
    </div>
  );
};
