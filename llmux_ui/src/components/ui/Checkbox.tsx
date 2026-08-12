// 马卡龙自定义复选框：圆角方框 + 主题色选中态 + 柔和阴影
import React from "react";
import { Check } from "lucide-react";
import { cn } from "@/utils/helpers";

interface CheckboxProps {
  checked: boolean;
  onChange: (checked: boolean) => void;
  label?: string;
  disabled?: boolean;
  className?: string;
}

export const Checkbox: React.FC<CheckboxProps> = ({
  checked,
  onChange,
  label,
  disabled,
  className,
}) => (
  <button
    type="button"
    role="checkbox"
    aria-checked={checked}
    disabled={disabled}
    onClick={() => onChange(!checked)}
    className={cn(
      "group flex items-center gap-2.5 text-left",
      disabled ? "cursor-not-allowed opacity-50" : "cursor-pointer",
      className,
    )}
  >
    <span
      className={cn(
        "flex h-[18px] w-[18px] shrink-0 items-center justify-center rounded-md border-2 transition-all duration-150",
        checked
          ? "border-primary bg-primary text-primary-foreground shadow-soft"
          : "border-border bg-card group-hover:border-primary/60",
      )}
    >
      {checked && <Check size={11} strokeWidth={3.5} />}
    </span>
    {label && (
      <span className="text-sm text-card-foreground">{label}</span>
    )}
  </button>
);
