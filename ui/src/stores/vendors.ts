import { create } from 'zustand';

export interface Vendor {
  id: string;
  name: string;
  protocol: string;
  default_base_url: string | null;
  default_anthropic_url: string | null;
  builtin: number;
  created_at: string;
}

interface VendorsState {
  vendors: Vendor[];
  isLoading: boolean;
  error: string | null;
  fetchVendors: () => Promise<void>;
  createVendor: (vendor: { id: string; name: string; protocol: string; default_base_url?: string; default_anthropic_url?: string }) => Promise<void>;
  updateVendor: (id: string, vendor: { name?: string; protocol?: string; default_base_url?: string; default_anthropic_url?: string }) => Promise<void>;
  deleteVendor: (id: string) => Promise<void>;
}

export const useVendorsStore = create<VendorsState>((set, get) => ({
  vendors: [],
  isLoading: false,
  error: null,

  fetchVendors: async () => {
    set({ isLoading: true, error: null });
    try {
      const res = await fetch('/api/vendors');
      if (!res.ok) throw new Error('Failed to fetch vendors');
      const data = await res.json();
      set({ vendors: data, isLoading: false });
    } catch (err: any) {
      set({ error: err.message, isLoading: false });
    }
  },

  createVendor: async (vendor) => {
    try {
      const res = await fetch('/api/vendors', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(vendor),
      });
      if (!res.ok) {
        const data = await res.json();
        throw new Error(data.error || 'Failed to create vendor');
      }
      await get().fetchVendors();
    } catch (err: any) {
      set({ error: err.message });
      throw err;
    }
  },

  updateVendor: async (id, vendor) => {
    try {
      const res = await fetch(`/api/vendors/${id}`, {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(vendor),
      });
      if (!res.ok) {
        const data = await res.json();
        throw new Error(data.error || 'Failed to update vendor');
      }
      await get().fetchVendors();
    } catch (err: any) {
      set({ error: err.message });
      throw err;
    }
  },

  deleteVendor: async (id) => {
    try {
      const res = await fetch(`/api/vendors/${id}`, { method: 'DELETE' });
      if (!res.ok) {
        const data = await res.json();
        throw new Error(data.error || 'Failed to delete vendor');
      }
      await get().fetchVendors();
    } catch (err: any) {
      set({ error: err.message });
      throw err;
    }
  },
}));
