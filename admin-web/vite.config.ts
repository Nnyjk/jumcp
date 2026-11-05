import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

// 管理台是后端 admin API 的薄前端（§4.4.5）：开发期用代理绕开 CORS，生产由 VITE_API_BASE 指定后端地址。
const BACKEND = 'http://127.0.0.1:8000'

export default defineConfig({
  plugins: [react()],
  server: {
    port: 5173,
    proxy: {
      '/admin': { target: BACKEND, changeOrigin: true },
      // SPA 自身也有 /health 路由：只有 XHR/fetch 才代理到后端，文档导航必须落到前端路由。
      '/health': {
        target: BACKEND,
        changeOrigin: true,
        bypass(request) {
          return request.headers.accept?.includes('text/html') ? '/index.html' : undefined
        },
      },
    },
  },
})
