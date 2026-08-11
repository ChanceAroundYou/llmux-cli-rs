// 应用布局：马卡龙悬浮侧边栏 + 全局顶栏 + 内容区
import React, { useState } from "react";
import { Outlet } from "react-router-dom";
import { Sidebar } from "./Sidebar";
import { TopBar } from "./TopBar";
import { ToastContainer } from "@/components/ui/Toast";

export const AppLayout: React.FC = () => {
  const [menuOpen, setMenuOpen] = useState(false);

  return (
    <div className="min-h-screen bg-background">
      <Sidebar open={menuOpen} onClose={() => setMenuOpen(false)} />
      <main className="min-h-screen lg:ml-[17rem]">
        <TopBar onMenuClick={() => setMenuOpen(true)} />
        <div className="mx-auto max-w-[1600px] px-4 py-6 lg:px-10 lg:py-8">
          <Outlet />
        </div>
      </main>
      <ToastContainer />
    </div>
  );
};
