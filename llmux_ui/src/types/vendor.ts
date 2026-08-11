// src/types/vendor.ts
export interface Vendor {
  id: string;
  name: string;
  protocol: string;
  protocols: string[];
  openai_responses: boolean;
  default_base_url?: string | null;
  default_anthropic_url?: string | null;
  coding_plan: number; // 0 | 1
  coding_base_url?: string | null;
  coding_anthropic_url?: string | null;
  builtin: number; // 0 | 1
  created_at?: string | null;
}

export interface VendorCreatePayload {
  id: string;
  name: string;
  protocol?: string;
  default_base_url?: string | null;
  default_anthropic_url?: string | null;
  protocols?: string[] | string;
  openai_responses?: boolean;
  coding_plan?: number;
  coding_base_url?: string | null;
  coding_anthropic_url?: string | null;
}

export type VendorUpdatePayload = Partial<VendorCreatePayload>;
