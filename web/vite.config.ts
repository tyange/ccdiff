import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// 개발 중에는 화면만 vite 가 주고 /api·/ws 는 실행 중인 worktable 로 넘긴다.
// WORKTABLE_URL=http://127.0.0.1:<포트> 로 그 서버를 가리킨다. 서버가 Origin 을 검사하므로 그 주소로 바꿔 보낸다.
const target = process.env.WORKTABLE_URL ?? "http://127.0.0.1:4318";

export default defineConfig({
  plugins: [react()],
  server: {
    host: "127.0.0.1",
    port: 4317,
    strictPort: true,
    proxy: {
      "/api": { target, changeOrigin: true, headers: { Origin: target } },
      "/ws": { target, changeOrigin: true, ws: true, headers: { Origin: target } },
    },
  },
  // 로컬에서만 쓰는 앱이라 번들 크기 경고 한도를 올린다(Monaco 가 들어오면 수 MB).
  build: { outDir: "dist", emptyOutDir: true, chunkSizeWarningLimit: 8000 },
});
