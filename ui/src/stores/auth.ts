import { create } from 'zustand'
import { apiFetch } from '@/lib/api'

interface AuthState {
  isAuthenticated: boolean | null
  username: string | null
  checkAuth: () => Promise<void>
  login: (username: string, password: string) => Promise<boolean>
  logout: () => Promise<void>
}

export const useAuthStore = create<AuthState>((set) => ({
  isAuthenticated: null,
  username: null,

  checkAuth: async () => {
    try {
      const res = await apiFetch('/api/auth/me')
      if (res.ok) {
        const data = await res.json()
        set({ isAuthenticated: true, username: data.username ?? null })
      } else {
        set({ isAuthenticated: false, username: null })
      }
    } catch {
      set({ isAuthenticated: false, username: null })
    }
  },

  login: async (username, password) => {
    const res = await apiFetch('/api/auth/login', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ username, password }),
    })
    if (res.ok) {
      const data = await res.json().catch(() => ({}))
      // if backend returns success, verify via me
      if (data.success !== false) {
        set({ isAuthenticated: true, username })
        return true
      }
    }
    return false
  },

  logout: async () => {
    try { await apiFetch('/api/auth/logout', { method: 'POST' }) } catch {}
    set({ isAuthenticated: false, username: null })
  },
}))
