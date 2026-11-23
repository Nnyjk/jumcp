/**
 * 管理台入口。
 * - 查询默认不重试、窗口重新聚焦时不自动刷新：401/403 重试没有意义，实时性由页面自己声明
 *   refetchInterval（目前只有工作台按开关轮询），避免切窗口就冲击管理面。
 * - BrowserRouter 依赖静态服务把未知路径回退到 index.html，否则刷新深链（如 /tools）会 404；
 *   /admin、/mcp、/health 这类后端路径不回退，拼错路径只会拿到 JSON 404。
 * - StrictMode 下组件会 mount → cleanup → mount：dialog、GSAP context 都在 cleanup 里还原，首屏动效不叠两层。
 */
import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { BrowserRouter } from 'react-router-dom'
import App from './App'
import { applyTheme, readThemePreference } from './theme'
import './styles/index.css'

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      retry: false,
      refetchOnWindowFocus: false,
    },
  },
})

// 首帧前先落 data-theme，避免浅色 token 闪一下再切深色。
applyTheme(readThemePreference())

const container = document.getElementById('root')
if (!container) throw new Error('缺少 #root 挂载点')

createRoot(container).render(
  <StrictMode>
    <QueryClientProvider client={queryClient}>
      <BrowserRouter>
        <App />
      </BrowserRouter>
    </QueryClientProvider>
  </StrictMode>,
)
