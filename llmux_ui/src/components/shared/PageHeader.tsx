// 页面标题区：马卡龙粉彩图标 + 标题 + 描述 + 操作区
import React from "react";
import { cn } from "@/utils/helpers";

interface PageHeaderProps {
  icon: React.ComponentType<{ className?: string }>;
  title: string;
  description?: React.ReactNode;
  actions?: React.ReactNode;
  iconClass?: string;
}

export const PageHeader: React.FC<PageHeaderProps> = ({
  icon: Icon,
  title,
  description,
  actions,
  iconClass = "bg-primary/20 text-primary-foreground",
}) => (
  <div className="flex flex-wrap items-center justify-between gap-3">
    <div className="flex items-center gap-3">
      <div
        className={cn(
          "flex h-11 w-11 shrink-0 items-center justify-center rounded-2xl shadow-soft",
          iconClass,
        )}
      >
        <Icon className="h-5 w-5" />
      </div>
      <div>
        <h1 className="text-2xl font-bold leading-tight">{title}</h1>
        {description && <p className="mt-0.5 text-sm text-muted-foreground">{description}</p>}
      </div>
    </div>
    {actions && <div className="flex items-center gap-2">{actions}</div>}
  </div>
);
