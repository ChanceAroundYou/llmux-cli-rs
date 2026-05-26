import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'
import path from 'path'

// https://vitejs.dev/config/
export default defineConfig({
  plugins: [
    react(),
    {
      name: 'sanitize-chunk-names',
      generateBundle(_, bundle) {
        const renames: Record<string, string> = {};
        for (const fileName of Object.keys(bundle)) {
          if (/[-_]ad(?=[a-f0-9]*[-_.])/.test(fileName)) {
            renames[fileName] = fileName.replace(/([-_])ad/, '$1x0');
          }
        }
        for (const [oldName, newName] of Object.entries(renames)) {
          bundle[oldName].fileName = newName;
        }
        for (const entry of Object.values(bundle)) {
          if (entry.type !== 'chunk') continue;
          for (const [oldName, newName] of Object.entries(renames)) {
            entry.code = entry.code.split(oldName).join(newName);
          }
        }
      },
    },
  ],
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "./src"),
    },
  },
  server: {
    port: 3000,
    proxy: {
      '/api': {
        target: 'http://localhost:25976',
        changeOrigin: true,
      },
      '/v1': {
        target: 'http://localhost:25976',
        changeOrigin: true,
      }
    }
  },
  build: {
    outDir: '../ui/dist',
    emptyOutDir: true,
    rollupOptions: {
      output: {
        manualChunks: {
          'vendor-lucide': ['lucide-react'],
          'vendor-react': ['react', 'react-dom', 'react-router-dom'],
        }
      }
    }
  }
})
