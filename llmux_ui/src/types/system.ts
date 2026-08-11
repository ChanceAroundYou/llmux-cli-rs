// src/types/system.ts
export interface SystemTools {
  version?: string;
  vscode: boolean;
  claude: boolean;
  gemini: boolean;
  opencode: boolean;
  codex: boolean;
}

export interface ClaudeSettings {
  exists: boolean;
  settings: Record<string, unknown> | null;
  error?: string;
}

export interface ClaudeSettingsPayload {
  apiBaseUrl: string;
  apiKey: string;
  opusModel?: string;
  sonnetModel?: string;
  haikuModel?: string;
}

export interface CodexSettings {
  exists: boolean;
  auth: Record<string, unknown> | null;
  configToml: string | null;
}

export interface CodexSettingsPayload {
  apiBaseUrl: string;
  apiKey: string;
  model?: string;
  reviewModel?: string;
  wireApi?: string;
  contextWindow?: number;
  autoCompactLimit?: number;
}

export interface GeminiSettings {
  exists: boolean;
  env: string | null;
  settings: string | null;
}

export interface GeminiSettingsPayload {
  apiKey: string;
  gatewayUrl: string;
  model?: string;
}

export interface BackupEntry {
  name: string;
  path: string;
  timestamp: string;
  size: number;
}

/** 应用/写入配置的统一返回 */
export interface ApplyResult {
  success: boolean;
  backupPath?: string;
  settings?: Record<string, unknown>;
  error?: string;
}
