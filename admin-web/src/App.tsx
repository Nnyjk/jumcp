import { lazy, Suspense, useEffect, useRef, useState } from 'react'
import { Navigate, Route, Routes, useLocation } from 'react-router-dom'
import { AuthProvider } from './auth/AuthContext'
import { useAuth } from './auth/session'
import { loginTarget, loginUrl } from './auth/redirect'
import { BackButton } from './components/BackButton'
import { LoadingState } from './components/PageShell'
import { Sidebar, type NavEntry } from './components/Sidebar'
import { TitleBar } from './components/TitleBar'
import DashboardPage from './pages/dashboard'
import LoginPage, { SessionSplash, SessionUnavailable } from './pages/login'
import { AdminViewContext, VIEW_TITLE, useAdminViewStack } from './viewStack'

// 路由级分包：工作台是入口页面保持静态，其余页面按需加载，避免单包超过 500 kB。
const AuditPage = lazy(() => import('./pages/audit'))
const AuditView = lazy(() => import('./pages/audit').then((module) => ({ default: module.AuditView })))
const AdminsPage = lazy(() => import('./pages/admins'))
const ExceptionPage = lazy(() => import('./pages/ExceptionPage'))
const GroupsPage = lazy(() => import('./pages/groups'))
const TokensPage = lazy(() => import('./pages/tokens'))
const ToolsPage = lazy(() => import('./pages/tools'))
const UpstreamsPage = lazy(() => import('./pages/upstreams'))
const CapabilityDossierPage = lazy(() => import('./pages/capability-dossier'))
const CapabilityDossier = lazy(() =>
  import('./pages/capability-dossier').then((module) => ({ default: module.CapabilityDossier })),
)

interface PageMeta extends NavEntry {
  title: string
}

/** 首页是工作台；路由元数据同时驱动侧栏与 TitleBar，避免标题两处维护。 */
const DASHBOARD: PageMeta = {
  to: '/',
  label: '工作台',
  icon: 'dashboard',
  title: '工作台',
}

const NAV_ITEMS: PageMeta[] = [
  DASHBOARD,
  { to: '/tools', label: '工具', icon: 'tools', title: '工具台账' },
  { to: '/upstreams', label: 'MCP 接入', icon: 'link', title: 'MCP 接入' },
  { to: '/groups', label: '组维护', icon: 'groups', title: '组维护' },
  { to: '/tokens', label: '访问令牌', icon: 'key', title: '访问令牌' },
  // 审核队列已并入审计页签：侧栏只保留一个入口，避免两个导航项指向同一路由。
  { to: '/audit', label: '审计', icon: 'audit', title: '审计' },
  { to: '/exceptions', label: '例外与可见性', icon: 'exception', title: '例外与可见性' },
  { to: '/admins', label: '账户管理', icon: 'account', title: '账户管理' },
]

const COLLAPSED_KEY = 'jumcp-admin-nav-collapsed'

/**
 * 会话闸门：未登录只能看见登录页，已登录才能进管理台外壳。
 * 管理面现在跑在自己的端口（52913），身份来自 HttpOnly cookie，不再由网关注入 Bearer。
 */
export default function App() {
  return (
    <AuthProvider>
      <AppGate />
    </AuthProvider>
  )
}

function AppGate() {
  const { session, retry } = useAuth()
  const location = useLocation()

  if (session.state === 'pending') return <SessionSplash />
  if (session.state === 'failed') {
    return <SessionUnavailable message={session.message} onRetry={retry} />
  }
  if (session.state === 'anonymous') {
    if (location.pathname === '/login') {
      return (
        <Routes>
          <Route path="/login" element={<LoginPage />} />
          <Route path="*" element={<Navigate to="/login" replace />} />
        </Routes>
      )
    }
    // 保留原始路径：`?from=` 便于刷新/分享，state.from 供无刷新回跳（两者同一份值）。
    return <Navigate to={loginUrl(location)} replace state={{ from: location }} />
  }
  if (location.pathname === '/login') {
    // 登录成功：回到被拦下的原始路径（`?from=` 或 history state），没有则进工作台。
    return <Navigate to={loginTarget(location)} replace />
  }
  return <AdminShell />
}

function AdminShell() {
  const { session, signOut } = useAuth()
  // AppGate 已保证此处是已登录态；用户名只用于侧栏展示，不参与任何鉴权判断。
  const username = session.state === 'authenticated' ? session.actor.id : ''
  const location = useLocation()
  const [collapsed, setCollapsed] = useState(() => {
    try {
      return window.localStorage.getItem(COLLAPSED_KEY) === '1'
    } catch {
      // 隐私模式/禁用存储时按未折叠渲染，不让整个管理台白屏。
      return false
    }
  })

  useEffect(() => {
    try {
      window.localStorage.setItem(COLLAPSED_KEY, collapsed ? '1' : '0')
    } catch {
      // 折叠偏好只是本地便利设置，写不进去不影响功能。
    }
  }, [collapsed])

  // 跳转链接只改变 hash 不移动键盘焦点：手动把焦点交给 main，下一次 Tab 才从正文开始。
  const focusMain = () => {
    document.getElementById('main-content')?.focus({ preventScroll: true })
  }

  const dossierRoute = location.pathname.startsWith('/capabilities/')
  const routeActive =
    NAV_ITEMS.find((item) =>
      dossierRoute
        ? item.to === '/tools'
        : item.to === '/'
          ? location.pathname === '/'
          : location.pathname.startsWith(item.to),
    ) ?? DASHBOARD
  const routeTitle = dossierRoute ? '能力档案' : routeActive.title

  const viewApi = useAdminViewStack(routeTitle)
  const { view, back, backLabel, clear, openDossier } = viewApi
  const title = view === null ? routeTitle : VIEW_TITLE[view.kind]

  // 进入/返回视图时内容换了一屏：滚回顶部，焦点交给 main，避免停在已消失的按钮上。
  const viewKey = view === null ? '' : JSON.stringify(view)
  const lastViewKey = useRef(viewKey)
  useEffect(() => {
    if (lastViewKey.current === viewKey) return
    lastViewKey.current = viewKey
    window.scrollTo({ top: 0 })
    document.getElementById('main-content')?.focus({ preventScroll: true })
  }, [viewKey])

  return (
    <AdminViewContext.Provider value={viewApi}>
      <div className="shell" data-collapsed={collapsed}>
        <a className="skip-link" href="#main-content" onClick={focusMain}>
          跳到主要内容
        </a>
        <Sidebar
          items={NAV_ITEMS}
          collapsed={collapsed}
          username={username}
          onToggleCollapsed={() => setCollapsed((value) => !value)}
          onNavigate={clear}
          onSignOut={() => {
            // 会话清空后闸门自动跳 /login（带 from），这里不重复导航以免两处竞速。
            void signOut()
          }}
        />
        <div className="shell-main">
          <TitleBar title={title} />
          <main className="content" id="main-content" tabIndex={-1}>
            <Suspense
              fallback={
                <div className="page">
                  <LoadingState rows={6} label="正在加载页面" />
                </div>
              }
            >
              {view === null ? (
                <Routes>
                  <Route path="/" element={<DashboardPage />} />
                  {/* 审核队列已合并进审计页签：保留旧深链，落到对应页签。 */}
                  <Route path="/review" element={<Navigate to="/audit?view=review" replace />} />
                  <Route path="/tools" element={<ToolsPage />} />
                  <Route path="/upstreams" element={<UpstreamsPage />} />
                  {/* 旧深链兼容：安装目录已并入「MCP 接入」页签。 */}
                  <Route path="/mcp-install" element={<Navigate to="/upstreams" replace />} />
                  <Route path="/capabilities/:id" element={<CapabilityDossierPage />} />
                  <Route path="/groups" element={<GroupsPage />} />
                  <Route path="/tokens" element={<TokensPage />} />
                  <Route path="/audit" element={<AuditPage />} />
                  <Route path="/exceptions" element={<ExceptionPage />} />
                  <Route path="/admins" element={<AdminsPage />} />
                  <Route path="*" element={<DashboardPage />} />
                </Routes>
              ) : view.kind === 'tools' ? (
                <>
                  <BackButton label={`返回${backLabel}`} onClick={back} />
                  <ToolsPage
                    initialGroup={view.group}
                    onOpenDossier={(id) => openDossier({ id })}
                  />
                </>
              ) : view.kind === 'dossier' ? (
                <CapabilityDossier
                  key={`dossier:${view.id}:${view.tab ?? ''}`}
                  id={view.id}
                  initialTab={view.tab}
                  onBack={back}
                  backLabel={`返回${backLabel}`}
                />
              ) : (
                <AuditView
                  key={`audit:${view.tokenId ?? ''}:${view.objectId ?? ''}:${view.eventId ?? ''}`}
                  initial={{
                    tokenId: view.tokenId,
                    objectId: view.objectId,
                    eventId: view.eventId,
                  }}
                  onBack={back}
                  backLabel={`返回${backLabel}`}
                />
              )}
            </Suspense>
          </main>
        </div>
      </div>
    </AdminViewContext.Provider>
  )
}
