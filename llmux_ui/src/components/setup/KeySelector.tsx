// 快速配置：网关密钥选择器（单选列表，无密钥时引导去创建）
import React from "react";
import { AlertCircle, ChevronRight, Key } from "lucide-react";
import { Link } from "react-router-dom";
import { useT } from "@/i18n";
import { cn } from "@/utils/helpers";
import type { ApiKey } from "@/types/key";

interface Props {
  keys: ApiKey[];
  selectedKeyId: number | "";
  onSelect: (id: number) => void;
}

export const KeySelector: React.FC<Props> = ({
  keys,
  selectedKeyId,
  onSelect,
}) => {
  const { t } = useT();

  if (keys.length === 0) {
    return (
      <div className="flex items-start gap-3 rounded-xl border border-dashed border-border bg-muted/30 p-4 text-sm text-muted-foreground">
        <AlertCircle size={15} className="mt-0.5 shrink-0 text-warning" />
        <span>
          {t("setup.noKeys")}{" "}
          <Link
            to="/keys"
            className="font-medium text-primary underline underline-offset-2"
          >
            {t("setup.createKey")}
          </Link>
        </span>
      </div>
    );
  }

  return (
    <div className="space-y-1.5">
      {keys.map((k) => {
        const active = selectedKeyId === k.id;
        return (
          <button
            key={k.id}
            onClick={() => onSelect(k.id!)}
            className={cn(
              "flex w-full items-center gap-3 rounded-xl border px-3 py-2.5 text-left transition-all",
              active
                ? "border-primary bg-primary/5"
                : "border-border bg-card hover:bg-muted/50",
            )}
          >
            <Key
              size={13}
              className={
                active ? "shrink-0 text-primary" : "shrink-0 text-muted-foreground"
              }
            />
            <div className="min-w-0 flex-1">
              <div className="truncate text-xs font-semibold">{k.name}</div>
              <div className="truncate font-mono text-xs text-muted-foreground">
                {k.key.slice(0, 12)}••••••••
              </div>
            </div>
            {active && (
              <ChevronRight size={12} className="shrink-0 text-primary" />
            )}
          </button>
        );
      })}
    </div>
  );
};
