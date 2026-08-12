// JSON 查看器：明暗自适应（token 化），文案走 i18n；展开/收起带高度动画
import React, { useState } from "react";
import { motion } from "framer-motion";
import { useT } from "@/i18n";

export const JsonViewer: React.FC<{ data: unknown }> = ({ data }) => {
  const { t } = useT();
  const [expanded, setExpanded] = useState(true);
  const json = JSON.stringify(data, null, 2);

  return (
    <div className="relative">
      <button
        onClick={() => setExpanded(!expanded)}
        className="absolute right-2 top-2 z-10 text-xs text-muted-foreground hover:text-foreground"
      >
        {expanded ? t("common.collapse") : t("common.expand")}
      </button>
      {/* height 在 auto 与折叠高度间平滑过渡；折叠时 overflow-auto 仍可滚动查看 */}
      <motion.pre
        initial={false}
        animate={{ height: expanded ? "auto" : 84, opacity: expanded ? 1 : 0.55 }}
        transition={{ duration: 0.25, ease: "easeOut" }}
        className="overflow-auto rounded-xl border border-border bg-muted p-4 font-mono text-xs text-foreground/90"
      >
        {json}
      </motion.pre>
    </div>
  );
};
