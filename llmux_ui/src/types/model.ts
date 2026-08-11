// src/types/model.ts
export interface AvailableModel {
  id: string;
  name: string;
  object: string;
  created: number;
  owned_by: string;
  error?: string;
  // 上游透传字段
  [key: string]: unknown;
}

export interface AvailableModelsResponse {
  data: AvailableModel[];
  stale: boolean;
  cached_at: number;
}

export interface AliasAccountSummary {
  id: number;
  name: string;
  vendor_id: string;
  vendor_name: string;
  protocol: string;
  is_preferred: boolean;
}

export interface AliasResponse {
  id: number;
  alias: string;
  target_model: string;
  vendor_id?: string | null;
  created_at?: string | null;
  preferred_account_id?: number | null;
  accounts: AliasAccountSummary[];
}

export interface AliasCreatePayload {
  alias: string;
  target_model: string;
  vendor_id?: string;
  account_ids?: number[] | string;
  preferred_account_id?: number;
}

export interface ModelTestPayload {
  model: string;
  vendorId?: string;
  accountId?: number;
}

export interface ModelTestResponse {
  success: boolean;
  latency: number;
  status: number;
  response: object | null;
  error: string | null;
}

export interface ModelTestAllPayload {
  models: Array<{ model: string; vendorId?: string }>;
}

export interface ModelTestAllResponse {
  success: boolean;
  message: string;
  total: number;
}

export interface TestQueueStatus {
  isRunning: boolean;
  total: number;
  current: number;
  progress: number; // 0-100
}

export interface ModelHealthEntry {
  account_id: number;
  vendor_id?: string | null;
  model: string;
  last_checked: number;
  success: number; // 0 | 1
  latency: number;
  error?: string | null;
  limits_cache: object | null;
  limits_cache_updated_at?: string | null;
  account_name?: string | null;
}
