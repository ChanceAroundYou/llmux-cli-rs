// 马卡龙输入框：继承原生 input 属性（支持 onKeyDown 等）
import React from "react";
import { cn } from "@/utils/helpers";

interface InputProps extends Omit<React.InputHTMLAttributes<HTMLInputElement>, "onChange"> {
  value: string;
  onChange: (value: string) => void;
}

export const Input: React.FC<InputProps> = ({
  value,
  onChange,
  type = "text",
  className,
  ...props
}) => (
  <input
    type={type}
    value={value}
    onChange={(e) => onChange(e.target.value)}
    className={cn(
      "w-full rounded-xl border border-input bg-card px-3 py-2 text-sm transition-colors",
      "placeholder:text-muted-foreground/60",
      "focus:border-primary focus:outline-none focus:ring-2 focus:ring-primary/30",
      "disabled:cursor-not-allowed disabled:bg-muted disabled:text-muted-foreground",
      className,
    )}
    {...props}
  />
);
