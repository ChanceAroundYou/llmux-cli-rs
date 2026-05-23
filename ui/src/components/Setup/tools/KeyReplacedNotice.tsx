import React from 'react';
import { Info } from 'lucide-react';

interface Props {
  notice: string | null;
  onDismiss: () => void;
}

export function KeyReplacedNotice({ notice, onDismiss }: Props) {
  if (!notice) return null;
  return (
    <div className="flex items-start gap-2 p-3 rounded-xl border border-blue-500/20 bg-blue-500/5 text-xs text-blue-600 dark:text-blue-400">
      <Info size={13} className="shrink-0 mt-0.5" />
      <span className="flex-1">{notice}</span>
      <button onClick={onDismiss} className="shrink-0 opacity-50 hover:opacity-100">✕</button>
    </div>
  );
}
