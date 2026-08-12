// 密码输入框：内置显示/隐藏切换，便于复制粘贴
import React, { useState } from "react";
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
        {show ? <EyeOff className="h-4 w-4" /> : <Eye className="h-4 w-4" />}
      </button>
    </div>
  );
};
