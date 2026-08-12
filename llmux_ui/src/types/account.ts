// src/types/account.ts
export interface AccountPublic {
  id?: number;
  vendor_id: string;
  name: string;
  base_url?: string | null;
  anthropic_base_url?: string | null;
  openai_compatible: number; // 0 | 1
  enabled: number; // 0 | 1
  weight: number;
  notes?: string | null;
  created_at?: string | null;
  /** 账户是否使用厂商的 Coding Plan 端点 */
  uses_coding?: number; // 0 | 1
}

export interface AccountCreatePayload {
  vendor_id: string;
  name: string;
  api_key: string;
  base_url?: string | null;
  anthropic_base_url?: string | null;
  enabled?: number;
  weight?: number;
  openai_compatible?: number;
  notes?: string | null;
  skip_validation?: boolean;
}

export interface AccountUpdatePayload extends Partial<AccountCreatePayload> {
  // api_key 传 "********" 或空 = 不改
}

export interface AccountCreateResponse {
  success: boolean;
  id: number;
  message: string;
  modelCount: number;
  skippedValidation: boolean;
}

export interface AccountUpdateResponse {
  success: boolean;
  message: string;
}

export interface AccountDeleteResponse {
  success: boolean;
  message: string;
}
