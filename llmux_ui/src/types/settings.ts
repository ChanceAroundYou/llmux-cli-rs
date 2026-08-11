// src/types/settings.ts
export type SettingsMap = Record<string, unknown>;

export interface ConfigExport {
  version: number;
  accounts: Array<{
    id: number;
    vendor_id: string;
    name: string;
    api_key: string;
    base_url?: string | null;
    anthropic_base_url?: string | null;
    openai_compatible: number;
    enabled: number;
    weight: number;
    notes?: string | null;
  }>;
  aliases: Array<{
    alias: string;
    target_model: string;
    vendor_id?: string | null;
    account_ids: number[];
    preferred_account_id?: number | null;
  }>;
  keys: Array<{
    name: string;
    key: string;
    allowed_models: string[];
  }>;
  settings: Array<{ key: string; value: string }>;
}

export interface ImportResponse {
  success: boolean;
  imported: {
    accounts: number;
    aliases: number;
    keys: number;
  };
}
