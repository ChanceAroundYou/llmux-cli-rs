// 快速配置：模型角色下拉（select + 百万上下文 [1m] 开关 + 实际写入值预览）
import React from "react";
import { ChevronDown } from "lucide-react";
import { useT } from "@/i18n";
import { cn } from "@/utils/helpers";

interface Props {
  label: string;
  envKey: string;
  models: string[];
  value: string;
  longContext?: boolean;
  onChange: (v: string) => void;
  onLongContextChange?: (v: boolean) => void;
}

export const ModelRoleSelect: React.FC<Props> = ({
  label,
  envKey,
  models,
  value,
  longContext = false,
  onChange,
  onLongContextChange,
}) => {
  const { t } = useT();
  const effectiveValue = value ? (longContext ? `${value}[1m]` : value) : "";

  return (
    <div className="space-y-1.5">
      <div className="flex items-center justify-between">
        <span className="text-xs font-semibold uppercase tracking-wide text-muted-foreground">
          {label}
        </span>
        <span className="font-mono text-xs text-muted-foreground/60">
          {envKey}
        </span>
      </div>
      <div className="flex gap-2">
        <div className="relative flex-1">
          <select
            value={value}
            onChange={(e) => onChange(e.target.value)}
            className="w-full appearance-none rounded-lg border border-border bg-card px-3 py-2 pr-7 text-xs font-medium transition-colors focus:border-primary focus:outline-none"
          >
            <option value="">{t("setup.noRole")}</option>
            {models.map((m) => (
              <option key={m} value={m}>
                {m}
              </option>
            ))}
          </select>
          <ChevronDown
            size={12}
            className="pointer-events-none absolute right-2 top-1/2 -translate-y-1/2 text-muted-foreground"
          />
        </div>
        {onLongContextChange && (
          <button
            type="button"
            disabled={!value}
            onClick={() => onLongContextChange(!longContext)}
            title="1m"
            className={cn(
              "shrink-0 rounded-lg border px-2 text-xs font-semibold transition-all",
              !value && "cursor-not-allowed opacity-30",
              value && longContext
                ? "border-primary/40 bg-primary/15 text-primary"
                : "border-border bg-card text-muted-foreground hover:border-muted-foreground/50",
            )}
          >
            1m
          </button>
        )}
      </div>
      {effectiveValue && (
        <div className="pl-1 font-mono text-xs text-muted-foreground/70">
          → <span className="text-foreground/80">{effectiveValue}</span>
        </div>
      )}
    </div>
  );
};
