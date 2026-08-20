import { create } from 'zustand';

export interface AvailableModel {
  id: string;
  name?: string;
  object: string;
  created: number;
  owned_by: string;
  error?: string;
  context_length?: number;
}

export interface ModelAlias {
  id: number;
  alias: string;
  target_model: string;
  provider_id: string | null;
  account_ids: string | null;
  preferred_account_id: number | null;
}

export interface Account {
  id: number;
  alias: string;
  provider_id: string;
  base_url: string | null;
  is_active: number;
}

export interface PerAccountMeta {
  updatedAt: number;
  error?: string | null;
  count: number;
}

interface ModelsState {
  availableModels: AvailableModel[];
  cachedAt: number | null;
  aliases: ModelAlias[];
  accounts: Account[];
  isLoading: boolean;
  streaming: boolean;
  perAccountMeta: Record<string, PerAccountMeta>;
  error: string | null;
  fetchModels: (force?: boolean) => Promise<void>;
  streamModels: (force?: boolean) => Promise<void>;
  fetchAliases: () => Promise<void>;
  fetchAccounts: () => Promise<void>;
  addAlias: (alias: string, targetModel: string, providerId?: string, accountIds?: number[], preferredAccountId?: number) => Promise<void>;
  deleteAlias: (id: number) => Promise<void>;
  testModel: (modelId: string, providerId?: string, accountId?: number) => Promise<{ success: boolean; error?: string; latency?: number }>;
  startTestQueue: (models: { model: string, providerId: string, accountId?: number }[]) => Promise<{ success: boolean; error?: string }>;
  fetchTestQueueStatus: () => Promise<{ isRunning: boolean; current: number; total: number; progress: number }>;
}

let streamAbort: AbortController | null = null;

function mergeAccountModels(prev: AvailableModel[], alias: string, incoming: AvailableModel[]): AvailableModel[] {
  const filtered = prev.filter(m => m.owned_by !== alias);
  return [...filtered, ...incoming];
}

export const useModelsStore = create<ModelsState>((set, get) => ({
  availableModels: [],
  cachedAt: null,
  aliases: [],
  accounts: [],
  isLoading: false,
  streaming: false,
  perAccountMeta: {},
  error: null,

  fetchModels: async (force = false) => {
    set({ isLoading: true, error: null });
    try {
      const url = force ? '/api/models/available?force=true' : '/api/models/available';
      const res = await fetch(url);
      if (!res.ok) throw new Error('Failed to fetch available models');
      const json = await res.json();
      const models = Array.isArray(json) ? json : (json.data || []);
      const cachedAt = json.cached_at ?? null;
      const perAccountMeta: Record<string, PerAccountMeta> = {};
      if (Array.isArray(json.per_account)) {
        for (const p of json.per_account) {
          perAccountMeta[p.alias] = { updatedAt: p.updated_at ?? 0, error: p.error ?? null, count: p.count ?? 0 };
        }
      }
      set({ availableModels: models, cachedAt, perAccountMeta, isLoading: false });
    } catch (err: any) {
      set({ error: err.message, isLoading: false });
    }
  },

  streamModels: async (force = false) => {
    // abort previous stream
    if (streamAbort) { try { streamAbort.abort(); } catch {} }
    const ac = new AbortController();
    streamAbort = ac;
    set({ streaming: true, error: null });
    try {
      const url = force ? '/api/models/available/stream?force=true' : '/api/models/available/stream';
      const res = await fetch(url, { headers: { Accept: 'text/event-stream' }, signal: ac.signal });
      if (!res.ok || !res.body) throw new Error(`Stream failed: ${res.status}`);
      const reader = res.body.getReader();
      const decoder = new TextDecoder();
      let buf = '';
      while (true) {
        const { done, value } = await reader.read();
        if (done) break;
        buf += decoder.decode(value, { stream: true });
        let sep: number;
        while ((sep = buf.indexOf('\n\n')) !== -1) {
          const frame = buf.slice(0, sep);
          buf = buf.slice(sep + 2);
          let event = 'message';
          let data = '';
          for (const line of frame.split('\n')) {
            if (line.startsWith('event:')) event = line.slice(6).trim();
            else if (line.startsWith('data:')) data += line.slice(5).trimStart();
          }
          if (!data) continue;
          let payload: any;
          try { payload = JSON.parse(data); } catch { continue; }
          if (event === 'snapshot') {
            const models: AvailableModel[] = payload.data || [];
            const meta: Record<string, PerAccountMeta> = {};
            if (Array.isArray(payload.per_account)) {
              for (const p of payload.per_account) meta[p.alias] = { updatedAt: p.updated_at ?? 0, error: p.error ?? null, count: p.count ?? 0 };
            }
            set({ availableModels: models, cachedAt: payload.cached_at ?? null, perAccountMeta: meta });
          } else if (event === 'account') {
            const alias: string = payload.alias;
            const incoming: AvailableModel[] = payload.models || [];
            const updatedAt: number = payload.updated_at ?? Math.floor(Date.now() / 1000);
            set(s => ({
              availableModels: mergeAccountModels(s.availableModels, alias, incoming),
              perAccountMeta: { ...s.perAccountMeta, [alias]: { updatedAt, error: payload.error ?? null, count: incoming.length } },
              cachedAt: Math.max(s.cachedAt ?? 0, updatedAt),
            }));
          } else if (event === 'done' || event === 'error') {
            // error event is also terminal for that account batch; don’t close yet unless done
            if (event === 'done') {
              // keep streaming false after done
            }
          }
        }
      }
    } catch (err: any) {
      if (err?.name === 'AbortError') {
        // cancelled by new stream
      } else {
        console.error('streamModels failed', err);
        // fallback to full fetch
        try { await get().fetchModels(force); } catch {}
        set({ error: err.message });
      }
    } finally {
      if (streamAbort === ac) streamAbort = null;
      set({ streaming: false, isLoading: false });
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

  addAlias: async (alias, targetModel, providerId, accountIds, preferredAccountId) => {
    try {
      const body: any = { alias, target_model: targetModel, provider_id: providerId };
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

  testModel: async (modelId, providerId, accountId) => {
    try {
      const res = await fetch('/api/models/test', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ model: modelId, providerId, accountId }),
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
