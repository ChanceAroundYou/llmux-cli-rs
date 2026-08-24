import { create } from 'zustand';
import { apiFetch } from "@/lib/api";
import { applyTheme } from "@/lib/theme";

interface SettingsState {
  config: Record<string, any>;
  isLoading: boolean;
  isInitialized: boolean;
  fetchSettings: () => Promise<void>;
  updateSettings: (newConfig: Record<string, any>) => Promise<void>;
}

export const useSettingsStore = create<SettingsState>((set, get) => ({
  config: {},
  isLoading: false,
  isInitialized: false,

  fetchSettings: async () => {
    if (get().isLoading) return;
    set({ isLoading: true });
    try {
      const res = await apiFetch('/api/settings');
      const data = await res.json();
      set({ config: data, isInitialized: true });
      
      // 同步到 localStorage 用于防止闪白
      if (data.theme) {
        localStorage.setItem('llmux-theme', data.theme);
        applyTheme(data.theme);
      }
    } catch (err) {
      console.error('Failed to fetch settings:', err);
    } finally {
      set({ isLoading: false });
    }
  },

  updateSettings: async (newConfig) => {
    set({ isLoading: true });
    try {
      await apiFetch('/api/settings', {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(newConfig),
      });
      
      if (newConfig.theme) {
        localStorage.setItem('llmux-theme', newConfig.theme);
        applyTheme(newConfig.theme);
      }

      set((state) => ({ config: { ...state.config, ...newConfig } }));
    } catch (err) {
      console.error('Failed to update settings:', err);
    } finally {
      set({ isLoading: false });
    }
  },
}));