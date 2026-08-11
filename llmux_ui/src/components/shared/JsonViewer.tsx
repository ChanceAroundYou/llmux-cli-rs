// JSON 查看器：明暗自适应（token 化），文案走 i18n
import React, { useState } from "react";
import { useT } from "@/i18n";

export const JsonViewer: React.FC<{ data: unknown }> = ({ data }) => {
  const { t } = useT();
  const [expanded, setExpanded] = useState(true);
  const json = JSON.stringify(data, null, 2);

  return (
    <div className="relative">
      <button
        onClick={() => setExpanded(!expanded)}
        className="absolute right-2 top-2 text-xs text-muted-foreground hover:text-foreground"
      >
        {expanded ? t("common.collapse") : t("common.expand")}
      </button>
      <pre
        className={`overflow-auto rounded-xl border border-border bg-muted p-4 font-mono text-xs text-foreground/90 ${expanded ? "max-h-96" : "max-h-20"}`}
      >
        {json}
      </pre>
    </div>
  );
};
