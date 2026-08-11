// 快速配置：还原备份时 key 被替换的提示条
import React from "react";
import { Info, X } from "lucide-react";

interface Props {
  notice: string | null;
  onDismiss: () => void;
}

export const KeyReplacedNotice: React.FC<Props> = ({ notice, onDismiss }) => {
  if (!notice) return null;
  return (
    <div className="flex items-start gap-2 rounded-xl border border-primary/20 bg-primary/5 p-3 text-xs text-primary">
      <Info size={13} className="mt-0.5 shrink-0" />
      <span className="flex-1">{notice}</span>
      <button
        onClick={onDismiss}
        className="shrink-0 opacity-50 transition-opacity hover:opacity-100"
      >
        <X size={13} />
      </button>
    </div>
  );
};
