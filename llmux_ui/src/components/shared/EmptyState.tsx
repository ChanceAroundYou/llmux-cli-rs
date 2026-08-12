// 空态：马卡龙图标 + 标题 + 描述
import React from "react";

interface EmptyStateProps {
  icon: React.ComponentType<{ className?: string }>;
  title: string;
  description?: string;
  /** 底部可选操作区（如失败态的"重试"按钮） */
  action?: React.ReactNode;
}

export const EmptyState: React.FC<EmptyStateProps> = ({ icon: Icon, title, description, action }) => (
  // 复用闲置的自定义动画：整体淡入 + 图标缓慢浮动，空态不再瞬间出现
  <div className="flex animate-fade-in flex-col items-center justify-center gap-2 py-10 text-center">
    <div className="flex h-12 w-12 items-center justify-center rounded-2xl bg-muted text-muted-foreground">
      <Icon className="h-6 w-6 animate-float" />
    </div>
    <p className="text-sm font-medium text-muted-foreground">{title}</p>
    {description && <p className="text-xs text-muted-foreground/70">{description}</p>}
    {action && <div className="mt-1">{action}</div>}
  </div>
);
