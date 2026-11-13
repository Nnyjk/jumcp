import { defineConfig, loadEnv, type Plugin, type ProxyOptions } from 'vite'
import react from '@vitejs/plugin-react'

// 管理台是后端 admin API 的薄前端（§4.4.5）：开发期用代理绕开 CORS，生产由 VITE_API_BASE 指定后端地址。
// 开发期目标地址允许用 JUMCP_DEV_API_TARGET 覆盖（一键脚本在后端非 8000 端口时需要）；
// 与 JUMCP_DEV_ADMIN_TOKEN 一样只在 Node 进程内可见，不得改成 VITE_* 变量。
const apiTarget = process.env.JUMCP_DEV_API_TARGET ?? 'http://127.0.0.1:8000'

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
          '生产环境由网关/BFF 在同源路径注入 Authorization（或改用会话 Cookie），前端不做任何凭证处理；' +
          '本地开发请用 JUMCP_DEV_ADMIN_TOKEN=... npm run dev。',
      )
    },
  }
}

/** 开发期代理注入管理面 Bearer：token 只存在于 Node 进程环境，不进浏览器 bundle。 */
const devAdminAuth: NonNullable<ProxyOptions['configure']> = (proxy) => {
  proxy.on('proxyReq', (proxyReq) => {
    const token = process.env.JUMCP_DEV_ADMIN_TOKEN
    if (token !== undefined && token.trim() !== '') {
      proxyReq.setHeader('authorization', `Bearer ${token}`)
    }
  })
}

export default defineConfig({
  plugins: [react(), adminCredentialGuard()],
  server: {
    // 本地开发监听所有网卡（便于容器/WSL/手机访问）；本机仍用 http://127.0.0.1:5173 访问。
    host: '0.0.0.0',
    port: 5173,
    proxy: {
      '/admin': { target: apiTarget, changeOrigin: true, configure: devAdminAuth },
      // MCP 接入：令牌页复制的 URL 是本页同源 /mcp，开发期必须转发到后端才可用。
      // 这里不注入管理面 Bearer：Authorization 原样透传客户端自己的 access token。
      '/mcp': { target: apiTarget, changeOrigin: true },
      // SPA 自身也有 /health 路由：只有 XHR/fetch 才代理到后端，文档导航必须落到前端路由。
      '/health': {
        target: apiTarget,
        changeOrigin: true,
        configure: devAdminAuth,
        bypass(request) {
          return request.headers.accept?.includes('text/html') ? '/index.html' : undefined
        },
      },
    },
  },
})
