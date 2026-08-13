// src/hooks/useToast.ts
import { create } from "zustand";
import { useMemo } from "react";

interface ToastItem {
  id: string;
  type: "success" | "error" | "info" | "warning";
  message: string;
  duration?: number;
}

interface ToastStore {
  toasts: ToastItem[];
  add: (toast: Omit<ToastItem, "id">) => void;
  remove: (id: string) => void;
}

// 生成 toast 唯一 id：crypto.randomUUID 仅在安全上下文（HTTPS/localhost）可用，
// 局域网 IP 走普通 HTTP 时回退到时间戳 + 随机串
function makeId(): string {
  try {
    if (typeof crypto.randomUUID === "function") return crypto.randomUUID();
  } catch {
    // 忽略并走 fallback
  }
  return `${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`;
}

export const useToastStore = create<ToastStore>((set) => ({
  toasts: [],
  add: (toast) => {
    const id = makeId();
    set((s) => ({ toasts: [...s.toasts, { ...toast, id }] }));
    setTimeout(() => {
      set((s) => ({ toasts: s.toasts.filter((t) => t.id !== id) }));
    }, toast.duration ?? 4000);
  },
  remove: (id) => set((s) => ({ toasts: s.toasts.filter((t) => t.id !== id) })),
}));

export const useToast = () => {
  const add = useToastStore((s) => s.add);
  // memo 化：返回稳定引用，避免 useCallback deps 引用导致重建
  return useMemo(
    () => ({
      success: (message: string) => add({ type: "success", message }),
      error: (message: string) => add({ type: "error", message }),
      info: (message: string) => add({ type: "info", message }),
      warning: (message: string) => add({ type: "warning", message }),
    }),
    [add],
  );
};
