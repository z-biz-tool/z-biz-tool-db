import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import path from "path";
import fs from "fs";

const SHARED_SRC = path.resolve(__dirname, "../z-biz-tool-shared/src");
const SHARED_STUB_DIR = path.resolve(__dirname, ".stub-shared");

// T-057：CI/独立仓库检出时如果 sibling z-biz-tool-shared 不存在，
// 自动产出一个最小可用 stub（仅导出一个空对象）。
// 真实环境会用 ../z-biz-tool-shared/src 真源；stub 仅防 CI 编译失败。
function ensureSharedStub() {
  if (fs.existsSync(SHARED_SRC)) return;
  if (!fs.existsSync(SHARED_STUB_DIR)) {
    fs.mkdirSync(SHARED_STUB_DIR, { recursive: true });
    fs.writeFileSync(
      path.join(SHARED_STUB_DIR, "index.ts"),
      "// 自动化生成的 stub：sibling z-biz-tool-shared 缺失时兜底\nexport {};\n",
    );
  }
}
ensureSharedStub();

export default defineConfig(async () => ({
  plugins: [react()],
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "./src"),
      "z-biz-tool-shared":
        fs.existsSync(SHARED_SRC)
          ? SHARED_SRC
          : SHARED_STUB_DIR,
    },
  },
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
  },
  envPrefix: ["VITE_", "TAURI_"],
  build: {
    target: process.env.TAURI_PLATFORM === "windows" ? "chrome105" : "safari13",
    minify: !process.env.TAURI_DEBUG ? "esbuild" : false,
    sourcemap: !!process.env.TAURI_DEBUG,
  },
}));