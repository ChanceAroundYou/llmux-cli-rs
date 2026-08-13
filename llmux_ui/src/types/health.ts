// src/types/health.ts
export interface HealthEntry {
  id: string; // "acc_{id}"
  name: string;
  status: "healthy" | "degraded" | "down" | "unknown";
  successCount: number; // 成功次数（不是时间戳）
  totalChecks: number;
}
