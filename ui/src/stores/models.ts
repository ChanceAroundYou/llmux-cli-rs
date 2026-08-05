import { create } from 'zustand';

export interface AvailableModel {
  id: string;
  name?: string;
  object: string;
  created: number;
  owned_by: string;
  error?: string;
}

export interface ModelAlias {
  id: number;
  alias: string;
  target_model: string;
  vendor_id: string | null;
  account_ids: number[];
  preferred_account_id: number | null;
  created_at: string | null;
}

export interface Account {
  id: number;
  vendor_id: string;
  name: string;
  base_url: string | null;
  enabled: number;
}

interface ModelsState {
  availableModels: AvailableModel[];
  cachedAt: number | null;
  aliases: ModelAlias[];
  accounts: Account[];
  isLoading: boolean;
  error: string | null;
  fetchModels: (force?: boolean) => Promise<void>;
  fetchAliases: () => Promise<void>;
  fetchAccounts: () => Promise<void>;
  addAlias: (alias: string, targetModel: string, vendorId?: string, accountIds?: number[], preferredAccountId?: number) => Promise<void>;
  deleteAlias: (id: number) => Promise<void>;
  testModel: (modelId: string, vendorId?: string, accountId?: number) => Promise<{ success: boolean; error?: string; latency?: number }>;
  startTestQueue: (models: { model: string, vendorId: string }[]) => Promise<{ success: boolean; error?: string }>;
  fetchTestQueueStatus: () => Promise<{ isRunning: boolean; current: number; total: number; progress: number }>;
}

export const useModelsStore = create<ModelsState>((set, get) => ({
  availableModels: [],
  cachedAt: null,
  aliases: [],
  accounts: [],
  isLoading: false,
  error: null,

  fetchModels: async (force = false) => {
    set({ isLoading: true, error: null });
    try {
      const url = force ? '/api/models/available?force=true' : '/api/models/available';
      const res = await fetch(url);
      if (!res.ok) throw new Error('Failed to fetch available models');
      const json = await res.json();
      // New format: { data: [...], stale: boolean }; fallback: plain array
      const models = Array.isArray(json) ? json : (json.data || []);
      const cachedAt = json.cached_at ?? null;
      set({ availableModels: models, cachedAt, isLoading: false });
    } catch (err: any) {
      set({ error: err.message, isLoading: false });
    }
  },

  fetchAliases: async () => {
    set({ error: null });
    try {
      const res = await fetch('/api/models/aliases');
      if (!res.ok) throw new Error('Failed to fetch aliases');
      const data = await res.json();
      set({ aliases: data });
    } catch (err: any) {
      set({ error: err.message });
    }
  },

  fetchAccounts: async () => {
    try {
      const res = await fetch('/api/accounts');
      if (!res.ok) throw new Error('Failed to fetch accounts');
      const data = await res.json();
      set({ accounts: data });
    } catch (err: any) {
      console.error('Failed to fetch accounts:', err.message);
    }
  },

  addAlias: async (alias, targetModel, vendorId, accountIds, preferredAccountId) => {
    try {
      const body: any = { alias, target_model: targetModel, vendor_id: vendorId };
      if (accountIds && accountIds.length > 0) {
        body.account_ids = accountIds;
      }
      if (preferredAccountId != null) {
        body.preferred_account_id = preferredAccountId;
      }
      const res = await fetch('/api/models/aliases', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body),
      });
      if (!res.ok) {
        const data = await res.json();
        throw new Error(data.error || 'Failed to add alias');
      }
      await get().fetchAliases();
    } catch (err: any) {
      set({ error: err.message });
      throw err;
    }
  },

  deleteAlias: async (id) => {
    try {
      const res = await fetch(`/api/models/aliases/${id}`, { method: 'DELETE' });
      if (!res.ok) throw new Error('Failed to delete alias');
      await get().fetchAliases();
    } catch (err: any) {
      set({ error: err.message });
      throw err;
    }
  },

  testModel: async (modelId, vendorId, accountId) => {
    try {
      const res = await fetch('/api/models/test', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ model: modelId, vendorId, accountId }),
      });
      return await res.json();
    } catch (err: any) {
      return { success: false, error: err.message };
    }
  },

  startTestQueue: async (models) => {
    try {
      const res = await fetch('/api/models/test-all', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ models }),
      });
      return await res.json();
    } catch (err: any) {
      return { success: false, error: err.message };
    }
  },

  fetchTestQueueStatus: async () => {
    try {
      const res = await fetch('/api/models/test-queue/status');
      return await res.json();
    } catch (err: any) {
      return { isRunning: false, progress: 0, current: 0, total: 0 };
    }
  }
}));
