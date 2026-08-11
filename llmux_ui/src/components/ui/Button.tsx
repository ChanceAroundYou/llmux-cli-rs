// 马卡龙按钮：粉彩语义色 + 圆润胶囊形
import React from "react";
import { cn } from "@/utils/helpers";
import { Loader2 } from "lucide-react";

type Variant = "primary" | "secondary" | "danger" | "ghost" | "outline";
type Size = "sm" | "md" | "lg";

interface ButtonProps extends React.ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: Variant;
  size?: Size;
  loading?: boolean;
  icon?: React.ReactNode;
}

const variantStyles: Record<Variant, string> = {
  primary: "bg-primary text-primary-foreground shadow-soft hover:bg-primary/90",
  secondary: "bg-secondary text-secondary-foreground hover:bg-secondary/80",
  danger: "bg-destructive text-destructive-foreground shadow-soft hover:bg-destructive/90",
  ghost: "text-muted-foreground hover:bg-muted hover:text-foreground",
  outline: "border border-border text-foreground hover:bg-muted",
};

const sizeStyles: Record<Size, string> = {
  sm: "px-3 py-1.5 text-xs",
  md: "px-4 py-2 text-sm",
  lg: "px-6 py-3 text-base",
};

export const Button: React.FC<ButtonProps> = ({
  variant = "primary",
  size = "md",
  loading = false,
  icon,
  className,
  children,
  disabled,
  ...props
}) => (
  <button
    className={cn(
      "inline-flex items-center justify-center gap-2 rounded-full font-medium transition-all duration-200",
      "focus:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background",
      "disabled:cursor-not-allowed disabled:opacity-60",
      variantStyles[variant],
      sizeStyles[size],
      className,
    )}
    disabled={disabled || loading}
    {...props}
  >
    {loading && <Loader2 className="h-4 w-4 animate-spin" />}
    {!loading && icon}
    {children}
  </button>
);
