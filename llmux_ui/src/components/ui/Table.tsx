// 马卡龙表格：卡片容器 + 骨架屏 loading
import React from "react";
import { cn } from "@/utils/helpers";
import { useT } from "@/i18n";

interface Column<T> {
  key: string;
  title: string;
  render?: (row: T) => React.ReactNode;
  width?: string;
  align?: "left" | "center" | "right";
}

interface TableProps<T> {
  columns: Column<T>[];
  data: T[];
  rowKey: (row: T) => string | number;
  loading?: boolean;
  empty?: React.ReactNode;
  onRowClick?: (row: T) => void;
}

export function Table<T>({
  columns,
  data,
  rowKey,
  loading,
  empty,
  onRowClick,
}: TableProps<T>) {
  const { t } = useT();

  if (loading) {
    return (
      <div className="space-y-2 p-4">
        {Array.from({ length: 5 }).map((_, i) => (
          <div key={i} className="h-12 animate-pulse rounded-xl bg-muted" />
        ))}
      </div>
    );
  }

  if (!data.length) {
    return (
      <div className="flex h-40 items-center justify-center text-muted-foreground">
        {empty ?? t("common.noData")}
      </div>
    );
  }

  return (
    <div className="overflow-x-auto rounded-xl border border-border bg-card">
      <table className="w-full text-sm">
        <thead>
          <tr className="border-b border-border bg-muted/50">
            {columns.map((col) => (
              <th
                key={col.key}
                className={cn(
                  "px-4 py-3 text-left text-xs font-semibold uppercase tracking-wide text-muted-foreground",
                  col.align === "right" && "text-right",
                  col.align === "center" && "text-center",
                )}
                style={{ width: col.width }}
              >
                {col.title}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {data.map((row) => (
            <tr
              key={rowKey(row)}
              className={cn(
                "border-b border-border/60 transition-colors last:border-0 hover:bg-muted/40",
                onRowClick && "cursor-pointer",
              )}
              onClick={() => onRowClick?.(row)}
            >
              {columns.map((col) => (
                <td key={col.key} className={cn("px-4 py-3 text-left", col.align === "right" && "text-right", col.align === "center" && "text-center")}>
                  {col.render
                    ? col.render(row)
                    : ((row as Record<string, unknown>)[col.key] as React.ReactNode)}
                </td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
