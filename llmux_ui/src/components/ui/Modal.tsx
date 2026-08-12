// 马卡龙弹窗：柔和遮罩 + 圆角卡片 + 淡入淡出动效
import React, { useEffect } from "react";
import { createPortal } from "react-dom";
import { AnimatePresence, motion } from "framer-motion";
import { cn } from "@/utils/helpers";
import { X } from "lucide-react";

interface ModalProps {
  open: boolean;
  onClose: () => void;
  title?: string;
  children: React.ReactNode;
  footer?: React.ReactNode;
  size?: "sm" | "md" | "lg" | "xl";
}

const sizeMap = {
  sm: "max-w-md",
  md: "max-w-lg",
  lg: "max-w-2xl",
  xl: "max-w-4xl",
};

export const Modal: React.FC<ModalProps> = ({
  open,
  onClose,
  title,
  children,
  footer,
  size = "md",
}) => {
  useEffect(() => {
    if (!open) return;
    const handler = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    document.addEventListener("keydown", handler);
    // 锁定背景滚动
    document.body.style.overflow = "hidden";
    return () => {
      document.removeEventListener("keydown", handler);
      document.body.style.overflow = "";
    };
  }, [open, onClose]);

  // 用 portal 挂到 body：脱离页面容器（避免容器 transform 使 fixed 遮罩相对容器定位而漏顶）
  return createPortal(
    <AnimatePresence>
      {open && (
        <>
          {/* 遮罩：独立 fixed 全屏，从视口顶边开始覆盖，避免顶部露缝 */}
          <motion.div
            key="overlay"
            className="fixed inset-0 z-[60] bg-black/40 backdrop-blur-sm"
            onClick={onClose}
            aria-hidden="true"
            initial={{ opacity: 0 }}
            animate={{ opacity: 1 }}
            exit={{ opacity: 0 }}
            transition={{ duration: 0.15, ease: "easeOut" }}
          />
          {/* 弹窗层：独立 fixed 居中，容器指针穿透让点击落到遮罩 */}
          <div className="pointer-events-none fixed inset-0 z-[61] flex items-center justify-center p-4">
            <motion.div
              key="dialog"
              role="dialog"
              aria-modal="true"
              initial={{ opacity: 0, y: 8, scale: 0.97 }}
              animate={{ opacity: 1, y: 0, scale: 1 }}
              exit={{ opacity: 0, y: 8, scale: 0.98 }}
              transition={{ duration: 0.18, ease: "easeOut" }}
              className={cn(
                "pointer-events-auto relative w-full rounded-2xl border border-border bg-card shadow-card",
                sizeMap[size],
              )}
            >
              {title && (
                <div className="flex items-center justify-between border-b border-border px-6 py-4">
                  <h2 className="text-lg font-semibold text-card-foreground">{title}</h2>
                  <button
                    onClick={onClose}
                    className="rounded-lg p-1 text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
                  >
                    <X className="h-5 w-5" />
                  </button>
                </div>
              )}
              <div className="max-h-[70vh] overflow-y-auto px-6 py-4">{children}</div>
              {footer && (
                <div className="flex justify-end gap-3 border-t border-border px-6 py-4">
                  {footer}
                </div>
              )}
            </motion.div>
          </div>
        </>
      )}
    </AnimatePresence>,
    document.body,
  );
};
