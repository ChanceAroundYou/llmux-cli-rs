// 马卡龙 Toast：粉彩语义底色 + 右侧滑入/滑出动效
import React from "react";
import { AnimatePresence, motion } from "framer-motion";
import { useToastStore } from "@/hooks/useToast";
import { cn } from "@/utils/helpers";
import { X, CheckCircle2, AlertCircle, Info, AlertTriangle } from "lucide-react";

const typeConfig = {
  success: {
    style: "border-success/30 bg-success/10 text-success-foreground",
    icon: CheckCircle2,
  },
  error: {
    style: "border-destructive/30 bg-destructive/10 text-destructive-foreground",
    icon: AlertCircle,
  },
  info: {
    style: "border-primary/30 bg-primary/10 text-primary-foreground",
    icon: Info,
  },
  warning: {
    style: "border-warning/30 bg-warning/10 text-warning-foreground",
    icon: AlertTriangle,
  },
};

export const ToastContainer: React.FC = () => {
  const toasts = useToastStore((s) => s.toasts);
  const remove = useToastStore((s) => s.remove);

  return (
    <div className="fixed bottom-4 right-4 z-[100] flex flex-col items-end gap-2">
      <AnimatePresence>
        {toasts.map((toast) => {
          const config = typeConfig[toast.type];
          const Icon = config.icon;
          return (
            <motion.div
              key={toast.id}
              initial={{ opacity: 0, x: 32 }}
              animate={{ opacity: 1, x: 0 }}
              exit={{ opacity: 0, x: 24 }}
              transition={{ duration: 0.2, ease: [0.22, 1, 0.36, 1] }}
              className={cn(
                "flex items-center gap-3 rounded-xl border px-4 py-3 text-sm shadow-card backdrop-blur-sm",
                config.style,
              )}
            >
              <Icon className="h-4 w-4 shrink-0" />
              <span className="flex-1">{toast.message}</span>
              <button
                onClick={() => remove(toast.id)}
                className="rounded p-0.5 opacity-50 hover:opacity-100 transition-opacity"
              >
                <X className="h-3.5 w-3.5" />
              </button>
            </motion.div>
          );
        })}
      </AnimatePresence>
    </div>
  );
};
