// 马卡龙分段 Tabs：胶囊容器 + 浮起激活项（layoutId 在标签间平滑滑动）
import React from "react";
import { motion } from "framer-motion";
import { cn } from "@/utils/helpers";

interface TabsProps {
  active: string;
  onChange: (key: string) => void;
  items: Array<{ key: string; label: string }>;
}

export const Tabs: React.FC<TabsProps> = ({ active, onChange, items }) => (
  <div className="relative flex flex-wrap gap-1 rounded-full bg-muted p-1">
    {items.map((item) => {
      const isActive = active === item.key;
      return (
        <button
          key={item.key}
          onClick={() => onChange(item.key)}
          className={cn(
            "relative rounded-full px-4 py-1.5 text-sm font-medium transition-colors duration-200",
            isActive ? "text-foreground" : "text-muted-foreground hover:text-foreground",
          )}
        >
          {isActive && (
            <motion.span
              layoutId="tabs-active"
              className="absolute inset-0 rounded-full bg-card shadow-soft"
              transition={{ type: "spring", stiffness: 500, damping: 35 }}
            />
          )}
          <span className="relative z-10">{item.label}</span>
        </button>
      );
    })}
  </div>
);
