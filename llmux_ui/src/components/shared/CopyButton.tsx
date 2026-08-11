// 复制按钮：点击后短暂显示对勾；剪贴板不可用时回退 execCommand
import React, { useState } from "react";
import { Copy, Check } from "lucide-react";
import { cn } from "@/utils/helpers";
import { useT } from "@/i18n";

export const CopyButton: React.FC<{ text: string; className?: string }> = ({ text, className }) => {
  const { t } = useT();
  const [copied, setCopied] = useState(false);

  const handleCopy = async () => {
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
      {copied ? <Check className="h-4 w-4 text-success" /> : <Copy className="h-4 w-4" />}
    </button>
  );
};
