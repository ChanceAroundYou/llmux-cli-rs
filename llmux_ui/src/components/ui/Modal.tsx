// 马卡龙弹窗：柔和遮罩 + 圆角卡片 + 淡入淡出动效
// 可访问性：焦点陷阱 + 初始焦点/焦点还原 + aria-labelledby + 输入中 Esc 不关 + 多弹窗滚动锁计数
import React, { useEffect, useId, useRef } from "react";
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

// 弹窗内可聚焦元素选择器（用于焦点陷阱）
const FOCUSABLE_SELECTOR =
  'a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';

// 模块级滚动锁引用计数：多个弹窗叠放时，先关的不提前释放 body 滚动
let scrollLockCount = 0;
function lockScroll() {
  scrollLockCount += 1;
  document.body.style.overflow = "hidden";
}
function unlockScroll() {
  scrollLockCount = Math.max(0, scrollLockCount - 1);
  if (scrollLockCount === 0) document.body.style.overflow = "";
}

export const Modal: React.FC<ModalProps> = ({
  open,
  onClose,
  title,
  children,
  footer,
  size = "md",
}) => {
  const dialogRef = useRef<HTMLDivElement>(null);
  // 打开时的触发元素，关闭后还原焦点
  const restoreFocusRef = useRef<Element | null>(null);
  const titleId = useId();

  useEffect(() => {
    if (!open) return;
    restoreFocusRef.current = document.activeElement;
    lockScroll();

    const dialog = dialogRef.current;
    // 初始焦点落在弹窗容器（不抢输入），键盘用户从 Tab 开始遍历
    dialog?.focus();

    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        // 在输入控件内按 Esc 不关弹窗，避免误关正在编辑的内容
        const target = e.target as HTMLElement | null;
        const tag = target?.tagName;
        if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT") return;
        onClose();
        return;
      }
      if (e.key !== "Tab" || !dialog) return;
      // 焦点陷阱：Tab / Shift+Tab 在弹窗内循环，不逃逸到背景
      const focusables = Array.from(dialog.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR)).filter(
        (el) => el.offsetParent !== null || el === document.activeElement,
      );
      if (focusables.length === 0) return;
      const first = focusables[0];
      const last = focusables[focusables.length - 1];
      const active = document.activeElement;
      if (e.shiftKey && (active === first || active === dialog)) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && active === last) {
        e.preventDefault();
        first.focus();
      }
    };

    document.addEventListener("keydown", onKeyDown);
    return () => {
      document.removeEventListener("keydown", onKeyDown);
      unlockScroll();
      // 焦点还原到打开前的元素
      const restore = restoreFocusRef.current;
      if (restore instanceof HTMLElement) restore.focus();
      restoreFocusRef.current = null;
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
              aria-labelledby={title ? titleId : undefined}
              tabIndex={-1}
              ref={dialogRef}
              initial={{ opacity: 0, y: 8, scale: 0.97 }}
              animate={{ opacity: 1, y: 0, scale: 1 }}
              exit={{ opacity: 0, y: 8, scale: 0.98 }}
              transition={{ duration: 0.18, ease: "easeOut" }}
              className={cn(
                "pointer-events-auto relative w-full rounded-2xl border border-border bg-card shadow-card focus:outline-none",
                sizeMap[size],
              )}
            >
              {title && (
                <div className="flex items-center justify-between border-b border-border px-6 py-4">
                  <h2 id={titleId} className="text-lg font-semibold text-card-foreground">
                    {title}
                  </h2>
                  <button
                    onClick={onClose}
                    className="rounded-lg p-1 text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
                    aria-label={title ?? "close"}
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
