// src/api/settings.ts
import client from "./client";
import type { SettingsMap, ImportResponse } from "@/types/settings";

export const settingsApi = {
  get: () => client.get<SettingsMap>("/settings").then((r) => r.data),

  update: (payload: SettingsMap) =>
    client.put("/settings", payload).then((r) => r.data),

  reset: () => client.post("/settings/reset").then((r) => r.data),

  exportConfig: () =>
    client.get<Blob>("/export", { responseType: "blob" }).then((r) => {
      const url = URL.createObjectURL(r.data);
      const a = document.createElement("a");
      a.href = url;
      a.download = `llmux-config-${Date.now()}.json`;
      a.click();
      URL.revokeObjectURL(url);
    }),

  importConfig: (config: Record<string, unknown>) =>
    client
      .post<ImportResponse>("/import", config, {
        headers: { "Content-Type": "application/json" },
      })
      .then((r) => r.data),
};
