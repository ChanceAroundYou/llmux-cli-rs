// 品牌 Logo：马卡龙渐变方块 + 白色闪电（与 favicon 同风格）
import React from "react";
import { Zap } from "lucide-react";
import { cn } from "@/utils/helpers";

interface LogoProps {
  size?: number;
  className?: string;
}

export const Logo: React.FC<LogoProps> = ({ size = 32, className }) => (
  <div
    className={cn(
      "flex shrink-0 items-center justify-center rounded-xl bg-gradient-to-br from-[#a78bfa] via-[#f9a8d4] to-[#7dd3fc] text-white shadow-soft",
      className,
    )}
    style={{ width: size, height: size }}
  >
    <Zap
      className="fill-current"
      strokeWidth={0}
      style={{ width: size * 0.55, height: size * 0.55 }}
    />
  </div>
);
