// 主题：明暗 scheme（system/light/dark）+ 色板 palette（多套马卡龙配色）
import { create } from "zustand";

export type Scheme = "system" | "light" | "dark";
export type Palette = "lavender" | "mint" | "peach" | "ocean" | "rose" | "azure";

const SCHEME_KEY = "llmux-theme";
const PALETTE_KEY = "llmux-palette";

/** 色板定义：id + i18n key + 预览色（设置页色块用） */
export interface PaletteDef {
  id: Palette;
  labelKey: string;
  color: string;
}

export const PALETTES: PaletteDef[] = [
  { id: "lavender", labelKey: "theme.palette.lavender", color: "#a78bfa" },
  { id: "mint", labelKey: "theme.palette.mint", color: "#6ee7b7" },
  { id: "peach", labelKey: "theme.palette.peach", color: "#fda4af" },
  { id: "ocean", labelKey: "theme.palette.ocean", color: "#7dd3fc" },
  { id: "rose", labelKey: "theme.palette.rose", color: "#f9a8d4" },
  { id: "azure", labelKey: "theme.palette.azure", color: "#3b82f6" },
];

export function getSystemTheme(): "light" | "dark" {
  return window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
}

function getInitialScheme(): Scheme {
  const saved = localStorage.getItem(SCHEME_KEY);
  return saved === "light" || saved === "dark" || saved === "system" ? saved : "system";
}

function getInitialPalette(): Palette {
  const saved = localStorage.getItem(PALETTE_KEY);
  return PALETTES.some((p) => p.id === saved) ? (saved as Palette) : "lavender";
}

interface ThemeState {
  scheme: Scheme;
  palette: Palette;
  setScheme: (scheme: Scheme) => void;
  setPalette: (palette: Palette) => void;
}

export const useThemeStore = create<ThemeState>((set) => ({
  scheme: getInitialScheme(),
  palette: getInitialPalette(),
  setScheme: (scheme) => {
    localStorage.setItem(SCHEME_KEY, scheme);
    set({ scheme });
  },
  setPalette: (palette) => {
    localStorage.setItem(PALETTE_KEY, palette);
    set({ palette });
  },
}));

/** 同步到 <html>：data-theme 选色板 + .dark 选明暗（Tailwind darkMode: class） */
export function applyThemeClass(resolved: "light" | "dark", palette: Palette) {
  const root = document.documentElement;
  root.dataset.theme = palette;
  root.classList.toggle("dark", resolved === "dark");
  root.style.colorScheme = resolved;
}
