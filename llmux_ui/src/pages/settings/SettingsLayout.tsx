// 设置页二级导航：General / CLI / Import-Export
import React from "react";
import { NavLink, Outlet } from "react-router-dom";
import { cn } from "@/utils/helpers";
import { useT } from "@/i18n";
import { PageHeader } from "@/components/shared/PageHeader";
import { Settings } from "lucide-react";

export const SettingsLayout: React.FC = () => {
  const { t } = useT();
  const items = [
    { path: "/settings", label: t("settings.title"), end: true },
    { path: "/settings/import-export", label: t("settings.importExport") },
  ];

  return (
    <div className="animate-fade-in space-y-6">
      <PageHeader
        icon={Settings}
        iconClass="bg-muted text-muted-foreground"
        title={t("settings.title")}
        description={t("settings.desc")}
      />
      <div className="flex flex-wrap gap-1 rounded-full bg-muted p-1">
        {items.map((item) => (
          <NavLink
            key={item.path}
            to={item.path}
            end={item.end}
            className={({ isActive }) =>
              cn(
                "rounded-full px-4 py-1.5 text-sm font-medium transition-all duration-200",
                isActive ? "bg-card text-foreground shadow-soft" : "text-muted-foreground hover:text-foreground",
              )
            }
          >
            {item.label}
          </NavLink>
        ))}
      </div>
      <Outlet />
    </div>
  );
};
