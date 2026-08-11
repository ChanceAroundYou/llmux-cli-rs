// 状态点：语义色 + 同色柔和发光
import React from "react";
import { cn } from "@/utils/helpers";

const colorMap: Record<string, string> = {
  healthy: "bg-success shadow-[0_0_8px_hsl(var(--success))]",
  degraded: "bg-warning shadow-[0_0_8px_hsl(var(--warning))]",
  down: "bg-destructive shadow-[0_0_8px_hsl(var(--destructive))]",
  unknown: "bg-muted-foreground",
};

export const StatusDot: React.FC<{ status: string; className?: string }> = ({
  status,
  className,
}) => (
  <span
    className={cn(
      "inline-block h-2.5 w-2.5 rounded-full",
      colorMap[status] ?? colorMap.unknown,
      className,
    )}
  />
);
