import { create } from 'zustand';

export interface Account {
  id: number;
  vendor_id: string;
  name: string;
  base_url: string | null;
  anthropic_base_url: string | null;
  enabled: number;
  weight: number;
  notes: string | null;
  created_at: string;
}

interface AccountsState {
  accounts: Account[];
  isLoading: boolean;
  error: string | null;
  fetchAccounts: () => Promise<void>;
  addAccount: (account: { vendor_id: string; name: string; api_key: string; base_url?: string; anthropic_base_url?: string; enabled?: number; weight?: number; notes?: string; skip_validation?: boolean }) => Promise<void>;
  updateAccount: (id: number, account: { vendor_id?: string; name?: string; api_key?: string; base_url?: string; anthropic_base_url?: string; enabled?: number; weight?: number; notes?: string; skip_validation?: boolean }) => Promise<void>;
  deleteAccount: (id: number) => Promise<void>;
  toggleEnabled: (id: number, currentStatus: number) => Promise<void>;
}

export const useAccountsStore = create<AccountsState>((set, get) => ({
  accounts: [],
  isLoading: false,
  error: null,

  fetchAccounts: async () => {
    set({ isLoading: true, error: null });
    try {
      const res = await fetch('/api/accounts');
      if (!res.ok) throw new Error('Failed to fetch accounts');
      const data = await res.json();
      set({ accounts: data, isLoading: false });
    } catch (err: any) {
      set({ error: err.message, isLoading: false });
    }
  },

  addAccount: async (account) => {
    try {
      const res = await fetch('/api/accounts', {
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
      const res = await fetch(`/api/accounts/${id}`, {
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
      const res = await fetch(`/api/accounts/${id}`, { method: 'DELETE' });
      if (!res.ok) throw new Error('Failed to delete account');
      await get().fetchAccounts();
    } catch (err: any) {
      set({ error: err.message });
      throw err;
    }
  },

  toggleEnabled: async (id, currentStatus) => {
    try {
      const res = await fetch(`/api/accounts/${id}`, {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ enabled: currentStatus === 1 ? 0 : 1 }),
      });
      if (!res.ok) throw new Error('Failed to update account');
      await get().fetchAccounts();
    } catch (err: any) {
      set({ error: err.message });
      throw err;
    }
  },
}));
