// 马卡龙卡片：柔和圆角 + 粉彩阴影
import React from "react";
import { cn } from "@/utils/helpers";

interface CardProps {
  title?: string;
  description?: string;
  actions?: React.ReactNode;
  children: React.ReactNode;
  className?: string;
}

export const Card: React.FC<CardProps> = ({
  title,
  description,
  actions,
  children,
  className,
}) => (
  <div className={cn("rounded-2xl border border-border bg-card shadow-card", className)}>
    {(title || actions) && (
      <div className="flex items-center justify-between gap-3 border-b border-border px-6 py-4">
        <div className="min-w-0">
          {title && (
            <h3 className="truncate text-base font-semibold text-card-foreground">{title}</h3>
          )}
          {description && (
            <p className="mt-0.5 text-sm text-muted-foreground">{description}</p>
          )}
        </div>
        {actions && <div className="shrink-0">{actions}</div>}
      </div>
    )}
    <div className="p-6">{children}</div>
  </div>
);
