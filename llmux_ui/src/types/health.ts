// src/types/health.ts
export interface HealthEntry {
  id: string; // "acc_{id}"
  name: string;
  status: "healthy" | "degraded" | "down" | "unknown";
  lastSuccess: number;
  totalChecks: number;
}
