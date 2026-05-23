export { cn } from '@/lib/utils';

export function parseAllowedModels(raw: string): string[] {
  if (!raw || raw === '*') return [];
  try {
    const parsed = JSON.parse(raw);
    return Array.isArray(parsed) ? parsed : [];
  } catch {
    return [];
  }
}
