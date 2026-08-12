// 应用布局：马卡龙悬浮侧边栏 + 全局顶栏 + 内容区（路由切换淡入）
import React, { useState } from "react";
import { Outlet, useLocation } from "react-router-dom";
import { motion } from "framer-motion";
import { Sidebar } from "./Sidebar";
import { TopBar } from "./TopBar";
import { ToastContainer } from "@/components/ui/Toast";

export const AppLayout: React.FC = () => {
  const [menuOpen, setMenuOpen] = useState(false);
  const location = useLocation();

  return (
    <div className="min-h-screen bg-background">
      <Sidebar open={menuOpen} onClose={() => setMenuOpen(false)} />
      <main className="min-h-screen lg:ml-[17rem]">
        <TopBar onMenuClick={() => setMenuOpen(true)} />
        <div className="mx-auto max-w-[1600px] px-4 py-6 lg:px-10 lg:py-8">
          {/* 路由切换：新页面淡入 + 上移 8px + 0.99 缩放，自定义 easeOut 曲线增强层次感。
              仅进入动画、旧页面立即替换，避免 AnimatePresence 退出层里的 <Outlet />
              提前渲染成新页面内容而造成的叠加/叠影。 */}
          <motion.div
            key={location.pathname}
            initial={{ opacity: 0, y: 8, scale: 0.99 }}
            animate={{ opacity: 1, y: 0, scale: 1 }}
            transition={{ duration: 0.4, ease: [0.22, 1, 0.36, 1] }}
          >
            <Outlet />
          </motion.div>
        </div>
      </main>
      <ToastContainer />
    </div>
  );
};
