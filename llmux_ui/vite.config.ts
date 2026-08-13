import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import path from "path";

export default defineConfig({
  plugins: [react()],
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "./src"),
    },
  },
  server: {
    port: 24444,
    proxy: {
      "/api": {
        target: "http://localhost:25999",
        changeOrigin: true,
      },
    },
  },
  build: {
    rollupOptions: {
      output: {
        // 把体积较大的第三方依赖拆成独立 chunk：利于浏览器长期缓存，
        // 也避免 react-diff-viewer（含语法高亮）等压在主包里
        manualChunks: {
          "vendor-react": ["react", "react-dom", "react-router-dom"],
          "vendor-motion": ["framer-motion"],
          "vendor-axios": ["axios"],
          "vendor-state": ["zustand"],
          "vendor-diff": ["react-diff-viewer-continued"],
        },
      },
    },
  },
});
