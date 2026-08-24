export type ThemeSetting = 'light' | 'dark' | 'system';

/** Apply a theme setting ('light' | 'dark' | 'system') to <html class="dark">. */
export function applyTheme(theme: string | undefined | null) {
  const resolved = theme || 'system';
  const mq = window.matchMedia('(prefers-color-scheme: dark)');
  const dark = resolved === 'dark' || (resolved === 'system' && mq.matches);
  document.documentElement.classList.toggle('dark', dark);
}

/** React to OS theme changes while the setting is "system". Returns a cleanup fn. */
export function watchSystemTheme(): () => void {
  const mq = window.matchMedia('(prefers-color-scheme: dark)');
  const onChange = () => {
    if ((localStorage.getItem('llmux-theme') || 'system') === 'system') {
      applyTheme('system');
    }
  };
  mq.addEventListener('change', onChange);
  return () => mq.removeEventListener('change', onChange);
}
