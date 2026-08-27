import { create } from 'zustand';
import { apiFetch } from "@/lib/api";

export interface Account {
  id: number;
  alias: string;
  provider_id: string;
  api_key?: string | null;
  base_url: string | null;
  is_active: number;
  weight: number;
  notes: string | null;
  openai_compatible: number | null;
  created_at: string;
  chat_endpoint: string | null;
  responses_endpoint: string | null;
  messages_endpoint: string | null;
  default_protocol: string | null;
  balance_provider: string | null;
  balance_auth: string | null;
}

interface AccountsState {
  accounts: Account[];
  isLoading: boolean;
  error: string | null;
  keys: Record<number, string>;
  fetchAccounts: () => Promise<void>;
  fetchAccountKey: (id: number) => Promise<string | null>;
  addAccount: (account: { alias: string; provider_id: string; api_key: string; chat_endpoint?: string | null; responses_endpoint?: string | null; messages_endpoint?: string | null; default_protocol?: string; balance_provider?: string; balance_auth?: string; openai_compatible?: number; skip_validation?: boolean }) => Promise<void>;
  updateAccount: (id: number, account: { alias?: string; provider_id?: string; api_key?: string; chat_endpoint?: string | null; responses_endpoint?: string | null; messages_endpoint?: string | null; default_protocol?: string; balance_provider?: string; balance_auth?: string; notes?: string; openai_compatible?: number; skip_validation?: boolean }) => Promise<void>;
  deleteAccount: (id: number) => Promise<void>;
  toggleActive: (id: number, currentStatus: number) => Promise<void>;
}

let _fetchedAt = 0;
let _inflight: Promise<void> | null = null;

export const useAccountsStore = create<AccountsState>((set, get) => ({
  accounts: [],
  isLoading: false,
  error: null,
  keys: {},

  fetchAccounts: async () => {
    const now = Date.now();
    if (now - _fetchedAt < 60_000 && get().accounts.length > 0) return;
    if (_inflight) return _inflight;
    _inflight = (async () => {
      set({ isLoading: true, error: null });
      try {
        const res = await apiFetch('/api/accounts');
        if (!res.ok) throw new Error('Failed to fetch accounts');
        const data = await res.json();
        set({ accounts: data, isLoading: false });
        _fetchedAt = Date.now();
      } catch (err: any) {
        set({ error: err.message, isLoading: false });
      } finally {
        _inflight = null;
      }
    })();
    return _inflight;
  },

  fetchAccountKey: async (id: number) => {
    const cached = get().keys[id];
    if (cached) return cached;
    try {
      const res = await apiFetch(`/api/accounts/${id}/key`);
      if (!res.ok) return null;
      const data = await res.json();
      const k: string = data.key ?? '';
      if (k) set(s => ({ keys: { ...s.keys, [id]: k } }));
      return k || null;
    } catch { return null; }
  },

  addAccount: async (account) => {
    try {
      const res = await apiFetch('/api/accounts', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(account),
      });
      if (!res.ok) {
        const data = await res.json();
        throw new Error(data.error || 'Failed to add account');
      }
      _fetchedAt = 0;
      await get().fetchAccounts();
    } catch (err: any) {
      set({ error: err.message });
      throw err;
    }
  },

  updateAccount: async (id, account) => {
    try {
      const res = await apiFetch(`/api/accounts/${id}`, {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(account),
      });
      if (!res.ok) {
        const data = await res.json();
        throw new Error(data.error || 'Failed to update account');
      }
      _fetchedAt = 0;
      if (account.api_key) {
        set(s => {
          const next = { ...s.keys };
          delete next[id];
          return { keys: next };
        });
      }
      await get().fetchAccounts();
    } catch (err: any) {
      set({ error: err.message });
      throw err;
    }
  },

  deleteAccount: async (id) => {
    try {
      const res = await apiFetch(`/api/accounts/${id}`, { method: 'DELETE' });
      if (!res.ok) throw new Error('Failed to delete account');
      _fetchedAt = 0;
      await get().fetchAccounts();
    } catch (err: any) {
      set({ error: err.message });
      throw err;
    }
  },

  toggleActive: async (id, currentStatus) => {
    try {
      const res = await apiFetch(`/api/accounts/${id}`, {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ is_active: currentStatus === 1 ? 0 : 1 }),
      });
      if (!res.ok) throw new Error('Failed to update account');
      _fetchedAt = 0;
      await get().fetchAccounts();
    } catch (err: any) {
      set({ error: err.message });
      throw err;
    }
  },
}));
