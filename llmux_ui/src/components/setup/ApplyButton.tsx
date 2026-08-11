// 快速配置：应用/初始化按钮 + 结果提示（成功含备份路径，失败显示错误）
import React from "react";
import { AlertCircle, Check, RotateCcw, Zap } from "lucide-react";
import { useT } from "@/i18n";
import { cn } from "@/utils/helpers";

export interface ApplyResult {
  success: boolean;
  backupPath?: string;
  error?: string;
}

interface Props {
  selectedKey: boolean;
  applying: boolean;
  settingsExists: boolean;
  applyResult: ApplyResult | null;
  onApply: () => void;
  applyLabel?: string;
  initLabel?: string;
}

export const ApplyButton: React.FC<Props> = ({
  selectedKey,
  applying,
  settingsExists,
  applyResult,
  onApply,
  applyLabel,
  initLabel,
}) => {
  const { t } = useT();
  return (
    <div className="space-y-2">
      <button
        onClick={onApply}
        disabled={!selectedKey || applying}
        className={cn(
          "flex w-full items-center justify-center gap-2 rounded-xl py-2.5 text-sm font-semibold transition-all",
          selectedKey && !applying
            ? "bg-primary text-primary-foreground hover:opacity-90"
            : "cursor-not-allowed bg-muted text-muted-foreground",
        )}
      >
        {applying ? (
          <>
            <RotateCcw
              size={14}
              className="animate-[spin_1s_linear_infinite_reverse]"
            />
            {t("setup.applying")}
          </>
        ) : (
          <>
            <Zap size={14} />
            {settingsExists
              ? (applyLabel ?? t("setup.applyBtn"))
              : (initLabel ?? t("setup.initBtn"))}
          </>
        )}
      </button>

      {applyResult && (
        <div
          className={cn(
            "space-y-1 rounded-xl border p-3 text-xs",
            applyResult.success
              ? "border-success/20 bg-success/10 text-success"
              : "border-destructive/20 bg-destructive/10 text-destructive",
          )}
        >
          {applyResult.success ? (
            <>
              <div className="flex items-center gap-1.5 font-semibold">
                <Check size={12} />
                {t("setup.applySuccess")}
              </div>
              {applyResult.backupPath && (
                <div className="break-all font-mono text-xs text-muted-foreground">
                  {t("setup.backupAt")}
                  {applyResult.backupPath}
                </div>
              )}
            </>
          ) : (
            <div className="flex items-center gap-1.5">
              <AlertCircle size={12} />
              {applyResult.error}
            </div>
          )}
        </div>
      )}
    </div>
  );
};
