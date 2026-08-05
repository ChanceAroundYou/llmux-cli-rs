export { cn } from '@/lib/utils';

export function parseAllowedModels(raw: string | string[]): string[] {
  if (!raw) return [];
  if (Array.isArray(raw)) return raw;
  if (raw === '*') return [];
  try {
    const parsed = JSON.parse(raw);
    return Array.isArray(parsed) ? parsed : [];
  } catch {
    return [];
  }
}
