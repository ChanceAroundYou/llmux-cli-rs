// 马卡龙开关：div 替代无效的 label>button，文本区点击也可切换
import React from "react";
import { cn } from "@/utils/helpers";

interface SwitchProps {
  label: string;
  description?: string;
  checked: boolean;
  onChange: (checked: boolean) => void;
  disabled?: boolean;
}

export const Switch: React.FC<SwitchProps> = ({
  label,
  description,
  checked,
  onChange,
  disabled,
}) => (
  <div className={cn("flex items-center gap-3", disabled ? "cursor-not-allowed opacity-60" : "cursor-pointer")}>
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      aria-label={label}
      disabled={disabled}
      onClick={() => onChange(!checked)}
      className={cn(
        "relative inline-flex h-6 w-10 shrink-0 items-center rounded-full transition-colors duration-200",
        checked ? "bg-primary" : "bg-muted-foreground/25",
      )}
    >
      <span
        className={cn(
          "inline-block h-4 w-4 rounded-full bg-white shadow-soft transition-transform duration-200",
          checked ? "translate-x-5" : "translate-x-1",
        )}
      />
    </button>
    <div className="flex-1" onClick={disabled ? undefined : () => onChange(!checked)}>
      <span className="text-sm font-medium text-card-foreground">{label}</span>
      {description && <p className="text-xs text-muted-foreground">{description}</p>}
    </div>
  </div>
);
