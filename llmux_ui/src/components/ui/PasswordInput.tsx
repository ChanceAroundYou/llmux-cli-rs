// 密码输入框：内置显示/隐藏切换，便于复制粘贴
import React, { useState } from "react";
import { AnimatePresence, motion } from "framer-motion";
import { cn } from "@/utils/helpers";
import { useT } from "@/i18n";
import { Input } from "./Input";
import { Eye, EyeOff } from "lucide-react";

interface Props {
  value: string;
  onChange: (value: string) => void;
  placeholder?: string;
  className?: string;
  disabled?: boolean;
}

export const PasswordInput: React.FC<Props> = ({ value, onChange, placeholder, className, disabled }) => {
  const { t } = useT();
  const [show, setShow] = useState(false);
  return (
    <div className={cn("relative", className)}>
      <Input
        type={show ? "text" : "password"}
        value={value}
        onChange={onChange}
        placeholder={placeholder}
        disabled={disabled}
        className="pr-10"
      />
      <button
        type="button"
        onClick={() => setShow(!show)}
        className="absolute right-2 top-1/2 -translate-y-1/2 rounded-lg p-1 text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
        aria-label={show ? t("common.hide") : t("common.show")}
      >
        {/* 眼睛图标：旋转交叉淡入 */}
        <AnimatePresence mode="wait" initial={false}>
          {show ? (
            <motion.span
              key="eye-off"
              initial={{ opacity: 0, rotate: -90 }}
              animate={{ opacity: 1, rotate: 0 }}
              exit={{ opacity: 0, rotate: 90 }}
              transition={{ duration: 0.15 }}
              className="flex"
            >
              <EyeOff className="h-4 w-4" />
            </motion.span>
          ) : (
            <motion.span
              key="eye"
              initial={{ opacity: 0, rotate: 90 }}
              animate={{ opacity: 1, rotate: 0 }}
              exit={{ opacity: 0, rotate: -90 }}
              transition={{ duration: 0.15 }}
              className="flex"
            >
              <Eye className="h-4 w-4" />
            </motion.span>
          )}
        </AnimatePresence>
      </button>
    </div>
  );
};
