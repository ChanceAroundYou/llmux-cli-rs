// src/types/key.ts
export interface ApiKey {
  id?: number;
  name: string;
  key: string;
  enabled: number; // 0 | 1
  last_used_at?: string | null;
  created_at?: string | null;
  allowed_models: string | string[]; // "*" 或模型名数组
}

export interface KeyCreatePayload {
  name?: string;
  allowed_models?: string | string[];
}

export interface KeyCreateResponse {
  success: boolean;
  id: number;
  key: string;
}

export interface KeyUpdatePayload {
  name?: string;
  allowed_models?: string | string[] | null;
}
