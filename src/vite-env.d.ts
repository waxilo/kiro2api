/// <reference types="vite/client" />
//
// 让 TypeScript 认识 Vite 的资源导入（`import './x.css'`）以及 `import.meta.env`。
// 缺了它，`tsc --noEmit` 会把 CSS 导入当成「找不到模块」。
