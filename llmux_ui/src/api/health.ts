// src/api/health.ts
import client from "./client";
import type { HealthEntry } from "@/types/health";

export const healthApi = {
  list: () => client.get<HealthEntry[]>("/health").then((r) => r.data),
};
