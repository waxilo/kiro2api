import { defineConfig } from 'vite';

// Tauri 期望固定端口，且失败时不自动切换（否则 Rust 侧 devUrl 对不上）。
export default defineConfig({
  clearScreen: false,
  server: {
    port: 1430,
    strictPort: true,
    watch: {
      // src-tauri 的变化由 tauri dev 自己监听，vite 不必重复扫描
      ignored: ['**/src-tauri/**'],
    },
  },
  build: {
    target: 'es2021',
    minify: 'esbuild',
    sourcemap: false,
  },
});
