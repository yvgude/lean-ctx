import react from "@vitejs/plugin-react";
import { defineConfig, loadEnv } from "vite";

export default defineConfig(({ mode }) => {
  const env = loadEnv(mode, process.cwd(), "VITE_");
  const target = env.VITE_LEANCTX_BASE_URL || "http://127.0.0.1:8080";

  return {
    plugins: [react()],
    server: {
      port: 5173,
      strictPort: true,
      proxy: {
        "/leanctx": {
          target,
          changeOrigin: true,
          rewrite: (path) => path.replace(/^\/leanctx/, ""),
        },
      },
    },
  };
});
