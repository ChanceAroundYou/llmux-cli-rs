// 马卡龙悬浮侧边栏：玻璃卡片 + 圆润导航 + 明暗双主题
import React, { useEffect, useState } from "react";
import { NavLink } from "react-router-dom";
import { cn } from "@/utils/helpers";
import { useT } from "@/i18n";
import { Logo } from "@/components/shared/Logo";
import { systemApi } from "@/api/system";
import {
  LayoutDashboard,
  Users,
  KeyRound,
  Boxes,
  Building2,
  Settings,
  Wrench,
} from "lucide-react";

const navGroups: {
  key: string;
  items: { path: string; labelKey: string; icon: React.ComponentType<{ className?: string }>; end?: boolean }[];
}[] = [
  {
    key: "nav.group.main",
    items: [
      { path: "/", labelKey: "nav.dashboard", icon: LayoutDashboard, end: true },
      { path: "/accounts", labelKey: "nav.accounts", icon: Users },
      { path: "/keys", labelKey: "nav.keys", icon: KeyRound },
      { path: "/models", labelKey: "nav.models", icon: Boxes },
      { path: "/vendors", labelKey: "nav.vendors", icon: Building2 },
    ],
  },
  {
    key: "nav.group.system",
    items: [
      { path: "/setup", labelKey: "nav.setup", icon: Wrench },
      { path: "/settings", labelKey: "nav.settings", icon: Settings },
    ],
  },
];

interface SidebarProps {
  open: boolean;
  onClose?: () => void;
}

export const Sidebar: React.FC<SidebarProps> = ({ open, onClose }) => {
  const { t } = useT();
  // 后端版本号（footer 展示，来自 /system/tools）
  const [version, setVersion] = useState<string | null>(null);
  useEffect(() => {
    systemApi
      .getTools()
      .then((d) => setVersion(d.version ?? null))
      .catch(() => {});
  }, []);

  return (
    <>
      {/* 移动端遮罩 */}
      {open && (
        <div className="fixed inset-0 z-40 bg-black/30 backdrop-blur-sm lg:hidden" onClick={onClose} />
      )}
      <aside
        className={cn(
          "fixed left-0 top-0 z-50 h-screen w-64 p-2",
          "transition-transform duration-300 ease-out",
          "lg:translate-x-0",
          open ? "translate-x-0" : "-translate-x-full",
        )}
      >
        <div className="flex h-full flex-col rounded-2xl border border-border bg-card/80 shadow-card backdrop-blur-xl">
          {/* Logo */}
          <div className="flex h-16 items-center gap-3 px-4">
            <Logo size={36} />
            <div className="leading-tight">
              <span className="block text-lg font-bold tracking-tight">LLMux</span>
              <span className="block text-[11px] text-muted-foreground">AI Gateway</span>
            </div>
          </div>

          {/* Navigation */}
          <nav className="mt-2 flex-1 space-y-4 overflow-y-auto px-3 pb-3">
            {navGroups.map((group) => (
              <div key={group.key}>
                <p className="mb-1.5 px-3 text-[11px] font-semibold uppercase tracking-widest text-muted-foreground">
                  {t(group.key)}
                </p>
                <div className="space-y-1">
                  {group.items.map((item) => (
                    <NavLink
                      key={item.path}
                      to={item.path}
                      end={item.end}
                      onClick={onClose}
                      className={({ isActive }) =>
                        cn(
                          "flex items-center gap-3 rounded-xl px-3 py-2.5 text-sm font-medium transition-all duration-200",
                          isActive
                            ? "bg-primary text-primary-foreground shadow-soft"
                            : "text-muted-foreground hover:bg-muted hover:text-foreground",
                        )
                      }
                    >
                      <item.icon className="h-[18px] w-[18px] shrink-0" />
                      {t(item.labelKey)}
                    </NavLink>
                  ))}
                </div>
              </div>
            ))}
          </nav>

          {/* Footer：真实版本号（来自后端） */}
          <div className="border-t border-border px-4 py-3">
            <div className="flex items-center gap-2 px-1">
              <span className="text-xs text-muted-foreground">
                {t("nav.footer")}
                {version ? ` · v${version}` : ""}
              </span>
            </div>
          </div>
        </div>
      </aside>
    </>
  );
};
