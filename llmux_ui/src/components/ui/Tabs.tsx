// 马卡龙分段 Tabs：胶囊容器 + 浮起激活项
import React from "react";
import { cn } from "@/utils/helpers";

interface TabsProps {
  active: string;
  onChange: (key: string) => void;
  items: Array<{ key: string; label: string }>;
}

export const Tabs: React.FC<TabsProps> = ({ active, onChange, items }) => (
  <div className="flex flex-wrap gap-1 rounded-full bg-muted p-1">
    {items.map((item) => (
      <button
        key={item.key}
        onClick={() => onChange(item.key)}
        className={cn(
          "rounded-full px-4 py-1.5 text-sm font-medium transition-all duration-200",
          active === item.key
            ? "bg-card text-foreground shadow-soft"
            : "text-muted-foreground hover:text-foreground",
        )}
      >
        {item.label}
      </button>
    ))}
  </div>
);
