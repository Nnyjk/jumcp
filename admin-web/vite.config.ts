import { defineConfig, loadEnv, type Plugin } from 'vite'
import react from '@vitejs/plugin-react'

// 管理台是后端 admin API 的薄前端（§4.4.5）：开发期用代理绕开 CORS，生产由 VITE_API_BASE 指定后端地址。
// 后端拆成两个端口：MCP 面（/mcp、/health，默认 8000）与管理面（/admin，默认 8080）。
// 两个目标都允许在 Node 进程内覆盖（一键脚本分别导出 JUMCP_DEV_API_TARGET 与 JUMCP_DEV_ADMIN_TARGET），不会进浏览器产物。
const apiTarget = process.env.JUMCP_DEV_API_TARGET ?? 'http://127.0.0.1:8000'
const adminTarget = process.env.JUMCP_DEV_ADMIN_TARGET ?? 'http://127.0.0.1:8080'

/** 管理面凭证绝不允许进前端产物：构建期发现这些变量就终止构建。 */
const FORBIDDEN_ENV = ['VITE_ADMIN_TOKEN', 'VITE_ADMIN_ACTOR'] as const

/** 构建期守卫：生产构建检测到管理面凭证变量即抛错，杜绝凭证被内联进 bundle。 */
function adminCredentialGuard(): Plugin {
  return {
    name: 'jumcp-admin-credential-guard',
    configResolved(config) {
      if (config.mode !== 'production') return
      const fileEnv = loadEnv(config.mode, process.cwd(), '')
      const leaked = FORBIDDEN_ENV.filter((key) => {
        const value = process.env[key] ?? fileEnv[key] ?? ''
        return value.trim() !== ''
      })
      if (leaked.length === 0) return
      throw new Error(
        `[jumcp-admin-web] 构建终止：检测到 ${leaked.join('、')} —— 管理面凭证不得内联进前端产物。` +
          '管理台身份来自登录接口下发的 HttpOnly 会话 cookie，前端不处理任何凭证。',
      )
    },
  }
}

export default defineConfig({
  plugins: [react(), adminCredentialGuard()],
  server: {
    // 本地开发监听所有网卡（便于容器/WSL/手机访问）；本机仍用 http://127.0.0.1:5173 访问。
    host: '0.0.0.0',
    port: 5173,
    proxy: {
      // 管理面在自己的端口（8080）：登录页 POST /admin/login 必须打到 adminTarget，而不是 MCP 面的 apiTarget。
      // 不再注入 Authorization：登录页拿到的会话 cookie 由浏览器随请求带上（credentials: 'include'）。
      // changeOrigin 必须保持 false：后端对 cookie 身份的非 GET 请求做同源校验（Origin 的
      // authority 必须等于 Host），改写 Host 会把浏览器的 5173 变成 8080，登录直接 403。
      // 键写成正则：字面量 '/admin' 会按前缀吃掉 SPA 路由 /admins（被代理到后端直接 404）。
      '^/admin(?:/|$)': { target: adminTarget, changeOrigin: false },
      // MCP 接入：令牌页复制的 URL 是本页同源 /mcp，开发期必须转发到后端才可用。
      // 这里不注入管理面 Bearer：Authorization 原样透传客户端自己的 access token。
      // 前缀 /mcp 同时会命中前端路由 /mcp-install：文档导航交回 SPA，只有 XHR/fetch 才代理。
      '/mcp': {
        target: apiTarget,
        changeOrigin: true,
        bypass(request) {
          return request.headers.accept?.includes('text/html') ? '/index.html' : undefined
        },
      },
      // SPA 自身也有 /health 路由：只有 XHR/fetch 才代理到后端，文档导航必须落到前端路由。
      '/health': {
        target: apiTarget,
        changeOrigin: true,
        bypass(request) {
          return request.headers.accept?.includes('text/html') ? '/index.html' : undefined
        },
      },
    },
  },
})
