// 厂商品牌 Logo：品牌主色渐变 + 缩写（国内厂商无现成图标库收录，用缩写方案）
import React from "react";
import { cn } from "@/utils/helpers";

interface BrandStyle {
  abbr: string;
  from: string;
  to: string;
}

// 内置厂商的品牌色与缩写；色相取自各家品牌主色，饱和度调柔和适配马卡龙
const VENDOR_BRANDS: Record<string, BrandStyle> = {
  openai: { abbr: "O", from: "hsl(160 84% 45%)", to: "hsl(160 84% 28%)" },
  anthropic: { abbr: "A", from: "hsl(16 55% 65%)", to: "hsl(16 55% 48%)" },
  gemini: { abbr: "G", from: "hsl(217 89% 62%)", to: "hsl(232 90% 42%)" },
  deepseek: { abbr: "DS", from: "hsl(232 99% 70%)", to: "hsl(232 99% 50%)" },
  zhipu: { abbr: "GLM", from: "hsl(220 85% 62%)", to: "hsl(220 85% 42%)" },
  siliconflow: { abbr: "SF", from: "hsl(255 85% 70%)", to: "hsl(255 85% 50%)" },
  zai: { abbr: "S", from: "hsl(268 85% 70%)", to: "hsl(268 85% 50%)" },
  huoshan: { abbr: "H", from: "hsl(8 90% 66%)", to: "hsl(8 90% 48%)" },
  dashscope: { abbr: "D", from: "hsl(22 100% 62%)", to: "hsl(22 100% 45%)" },
  kimi: { abbr: "K", from: "hsl(0 0% 26%)", to: "hsl(0 0% 8%)" },
  minimax: { abbr: "MM", from: "hsl(270 70% 64%)", to: "hsl(270 70% 48%)" },
  xiaomi_mimo: { abbr: "M", from: "hsl(25 95% 62%)", to: "hsl(25 95% 46%)" },
};

// 未收录厂商：取名称首字符 + 中性马卡龙紫
function fallbackBrand(name: string): BrandStyle {
  const first = (name || "?").charAt(0).toUpperCase();
  return { abbr: first, from: "hsl(258 60% 72%)", to: "hsl(258 60% 55%)" };
}

interface Props {
  id: string;
  name: string;
  size?: number;
}

export const VendorLogo: React.FC<Props> = ({ id, name, size = 44 }) => {
  const brand = VENDOR_BRANDS[id] ?? fallbackBrand(name);
  const abbrSize = brand.abbr.length > 1 ? size * 0.32 : size * 0.4;
  return (
    <div
      className={cn(
        "flex shrink-0 select-none items-center justify-center rounded-xl font-bold text-white shadow-soft",
      )}
      style={{
        width: size,
        height: size,
        background: `linear-gradient(135deg, ${brand.from}, ${brand.to})`,
        fontSize: abbrSize,
      }}
    >
      {brand.abbr}
    </div>
  );
};
