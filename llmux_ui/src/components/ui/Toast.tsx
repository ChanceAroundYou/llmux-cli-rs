// 马卡龙 Toast：粉彩语义底色 + 右侧滑入动效
import React from "react";
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

  if (!toasts.length) return null;

  return (
    <div className="fixed bottom-4 right-4 z-[100] space-y-2">
      {toasts.map((toast) => {
        const config = typeConfig[toast.type];
        const Icon = config.icon;
        return (
          <div
            key={toast.id}
            className={cn(
              "flex items-center gap-3 rounded-xl border px-4 py-3 text-sm shadow-card backdrop-blur-sm animate-slide-in-right",
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
          </div>
        );
      })}
    </div>
  );
};
