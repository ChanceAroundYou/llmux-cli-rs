import { create } from 'zustand';

export interface ApiKey {
  id: number;
  name: string;
  key: string;
  enabled: number;
  last_used_at: string | null;
  created_at: string;
  allowed_models: string | string[];
}

interface KeysState {
  keys: ApiKey[];
  isLoading: boolean;
  fetchKeys: () => Promise<void>;
  createKey: (name: string, allowedModels: string[] | '*') => Promise<string>;
  deleteKey: (id: number) => Promise<void>;
  updateKey: (id: number, name: string, allowedModels: string[] | '*') => Promise<void>;
}

export const useKeysStore = create<KeysState>((set, get) => ({
  keys: [],
  isLoading: false,
  fetchKeys: async () => {
    set({ isLoading: true });
    try {
      const res = await fetch('/api/keys');
      if (res.ok) {
        const data = await res.json();
        set({ keys: data });
      }
    } finally {
      set({ isLoading: false });
    }
  },
  createKey: async (name, allowedModels) => {
    const res = await fetch('/api/keys', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ name, allowed_models: allowedModels }),
    });
    if (!res.ok) throw new Error('Failed to create API key');
    const data = await res.json();

    await get().fetchKeys();
    return data.key;
  },
  deleteKey: async (id) => {
    const res = await fetch(`/api/keys/${id}`, { method: 'DELETE' });
    if (!res.ok) throw new Error('Failed to delete API key');
    await get().fetchKeys();
  },
  updateKey: async (id, name, allowedModels) => {
    const res = await fetch(`/api/keys/${id}`, {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ name, allowed_models: allowedModels }),
    });
    if (!res.ok) throw new Error('Failed to update API key');
    await get().fetchKeys();
  },
}));
