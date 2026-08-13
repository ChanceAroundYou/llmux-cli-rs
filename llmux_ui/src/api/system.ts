// src/api/system.ts
import client from "./client";
import type {
  SystemTools,
  ClaudeSettings,
  ClaudeSettingsPayload,
  CodexSettings,
  CodexSettingsPayload,
  GeminiSettings,
  GeminiSettingsPayload,
  ApplyResult,
} from "@/types/system";

export const systemApi = {
  getTools: () => client.get<SystemTools>("/system/tools").then((r) => r.data),

  // Claude
  getClaudeSettings: () =>
    client.get<ClaudeSettings>("/system/claude-settings").then((r) => r.data),
  applyClaudeSettings: (payload: ClaudeSettingsPayload) =>
    client.post<ApplyResult>("/system/claude-settings", payload).then((r) => r.data),

  // Codex
  getCodexSettings: () =>
    client.get<CodexSettings>("/system/codex-settings").then((r) => r.data),
  applyCodexSettings: (payload: CodexSettingsPayload) =>
    client.post<ApplyResult>("/system/codex-settings", payload).then((r) => r.data),
  previewCodexSettings: (payload: CodexSettingsPayload) =>
    client
      .post<{ auth: Record<string, unknown> | null; configToml: string }>(
        "/system/codex-preview",
        payload,
      )
      .then((r) => r.data),

  // Gemini
  getGeminiSettings: () =>
    client.get<GeminiSettings>("/system/gemini-settings").then((r) => r.data),
  applyGeminiSettings: (payload: GeminiSettingsPayload) =>
    client.post<ApplyResult>("/system/gemini-settings", payload).then((r) => r.data),
  previewGeminiSettings: (payload: GeminiSettingsPayload) =>
    client
      .post<{ env: string; settings: string }>("/system/gemini-preview", payload)
      .then((r) => r.data),

  // Backups (generic)
  getBackups: (tool: "claude" | "codex" | "gemini", name?: string) =>
    client
      .get(`/system/${tool}-backups`, { params: name ? { name } : undefined })
      .then((r) => r.data),
  createBackup: (tool: "claude" | "codex" | "gemini", name: string) =>
    client.post(`/system/${tool}-backups`, { name }).then((r) => r.data),
  deleteBackup: (tool: "claude" | "codex" | "gemini", name: string) =>
    client
      .delete(`/system/${tool}-backups`, { data: { name } })
      .then((r) => r.data),
};
