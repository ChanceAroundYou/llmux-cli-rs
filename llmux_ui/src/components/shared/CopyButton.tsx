// 复制按钮：点击后短暂显示对勾；剪贴板不可用时回退 execCommand
import React, { useState } from "react";
import { AnimatePresence, motion } from "framer-motion";
import { Copy, Check } from "lucide-react";
import { cn } from "@/utils/helpers";
import { useT } from "@/i18n";

export const CopyButton: React.FC<{ text: string; className?: string }> = ({ text, className }) => {
  const { t } = useT();
  const [copied, setCopied] = useState(false);

  const handleCopy = async (e: React.MouseEvent<HTMLButtonElement>) => {
    // 阻止冒泡：复制按钮嵌套在可点击卡片内时（如别名卡片 onClick 打开编辑），
    // 点击复制不应触发父级动作
    e.stopPropagation();
    try {
      await navigator.clipboard.writeText(text);
    } catch {
      // 非 HTTPS / 无权限时回退到 execCommand
      const ta = document.createElement("textarea");
      ta.value = text;
      ta.style.position = "fixed";
      ta.style.opacity = "0";
      document.body.appendChild(ta);
      ta.select();
      try {
        document.execCommand("copy");
      } finally {
        document.body.removeChild(ta);
      }
    }
    setCopied(true);
    setTimeout(() => setCopied(false), 2000);
  };

  return (
    <button
      onClick={handleCopy}
      className={cn(
        "rounded-lg p-1 text-muted-foreground transition-colors hover:bg-muted hover:text-foreground",
        className,
      )}
      title={t("common.copy")}
      aria-label={t("common.copy")}
    >
      {/* 图标交叉切换：对勾以 scale + 旋转弹出，复制成功有奖励感 */}
      <AnimatePresence mode="wait" initial={false}>
        {copied ? (
          <motion.span
            key="check"
            initial={{ scale: 0, rotate: -30, opacity: 0 }}
            animate={{ scale: 1, rotate: 0, opacity: 1 }}
            exit={{ scale: 0, opacity: 0 }}
            transition={{ type: "spring", stiffness: 500, damping: 25 }}
            className="flex"
          >
            <Check className="h-4 w-4 text-success" />
          </motion.span>
        ) : (
          <motion.span
            key="copy"
            initial={{ scale: 0.6, opacity: 0 }}
            animate={{ scale: 1, opacity: 1 }}
            exit={{ scale: 0.6, opacity: 0 }}
            transition={{ duration: 0.12 }}
            className="flex"
          >
            <Copy className="h-4 w-4" />
          </motion.span>
        )}
      </AnimatePresence>
    </button>
  );
};
