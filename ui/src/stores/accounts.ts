import { create } from 'zustand';
import { apiFetch } from "@/lib/api";

export interface Account {
  id: number;
  alias: string;
  provider_id: string;
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
}

interface AccountsState {
  accounts: Account[];
  isLoading: boolean;
  error: string | null;
  fetchAccounts: () => Promise<void>;
  addAccount: (account: { alias: string; provider_id: string; api_key: string; chat_endpoint?: string | null; responses_endpoint?: string | null; messages_endpoint?: string | null; default_protocol?: string; openai_compatible?: number; skip_validation?: boolean }) => Promise<void>;
  updateAccount: (id: number, account: { alias?: string; provider_id?: string; api_key?: string; chat_endpoint?: string | null; responses_endpoint?: string | null; messages_endpoint?: string | null; default_protocol?: string; notes?: string; openai_compatible?: number; skip_validation?: boolean }) => Promise<void>;
  deleteAccount: (id: number) => Promise<void>;
  toggleActive: (id: number, currentStatus: number) => Promise<void>;
}

export const useAccountsStore = create<AccountsState>((set, get) => ({
  accounts: [],
  isLoading: false,
  error: null,

  fetchAccounts: async () => {
    set({ isLoading: true, error: null });
    try {
      const res = await apiFetch('/api/accounts');
      if (!res.ok) throw new Error('Failed to fetch accounts');
      const data = await res.json();
      set({ accounts: data, isLoading: false });
    } catch (err: any) {
      set({ error: err.message, isLoading: false });
    }
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
      await get().fetchAccounts();
    } catch (err: any) {
      set({ error: err.message });
      throw err;
    }
  },
}));
