// 马卡龙下拉：自定义箭头（离右边缘留出间距）+ 粉彩样式
import React from "react";
import { cn } from "@/utils/helpers";
import { ChevronDown } from "lucide-react";

interface SelectProps {
  value: string;
  onChange: (value: string) => void;
  options: Array<{ value: string; label: string }>;
  placeholder?: string;
  disabled?: boolean;
  className?: string;
}

export const Select: React.FC<SelectProps> = ({
  value,
  onChange,
  options,
  placeholder,
  disabled,
  className,
}) => (
  <div className={cn("group relative w-full", className)}>
    <select
      value={value}
      onChange={(e) => onChange(e.target.value)}
      disabled={disabled}
      className={cn(
        "w-full appearance-none rounded-xl border border-input bg-card py-2 pl-3 pr-9 text-sm transition-[color,border-color,box-shadow]",
        "focus:border-primary focus:outline-none focus:ring-2 focus:ring-primary/30",
        "disabled:cursor-not-allowed disabled:bg-muted disabled:text-muted-foreground",
      )}
    >
      {placeholder && <option value="">{placeholder}</option>}
      {options.map((opt) => (
        <option key={opt.value} value={opt.value}>
          {opt.label}
        </option>
      ))}
    </select>
    {/* 自定义箭头：pointer-events-none 避免遮挡点击；right-3 距右缘 12px；聚焦时旋转 */}
    <ChevronDown className="pointer-events-none absolute right-3 top-1/2 h-4 w-4 -translate-y-1/2 text-muted-foreground transition-transform duration-200 group-focus-within:rotate-180" />
  </div>
);
