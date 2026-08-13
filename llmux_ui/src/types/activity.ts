// src/types/activity.ts
export interface ActivityEntry {
  id: number;
  timestamp: number; // unix 毫秒
  model: string;
  success: number; // 0 | 1
  latency_ms: number;
  error_message?: string | null;
  account_name?: string | null;
}

export interface ActivityResponse {
  entries: ActivityEntry[];
  totalRequests: number;
  successCount: number;
}
