// 马卡龙徽标：粉彩语义底色
import React from "react";
import { cn } from "@/utils/helpers";

type BadgeVariant = "success" | "warning" | "danger" | "info" | "neutral";

const styles: Record<BadgeVariant, string> = {
  success: "bg-success/15 text-success-foreground",
  warning: "bg-warning/15 text-warning-foreground",
  danger: "bg-destructive/15 text-destructive-foreground",
  info: "bg-primary/15 text-primary-foreground",
  neutral: "bg-muted text-muted-foreground",
};

export const Badge: React.FC<{
  variant?: BadgeVariant;
  className?: string;
  children: React.ReactNode;
}> = ({ variant = "neutral", className, children }) => (
  <span
    className={cn(
      "inline-flex items-center rounded-full px-2.5 py-0.5 text-xs font-medium",
      styles[variant],
      className,
    )}
  >
    {children}
  </span>
);
