import { useEffect, useRef, useState } from 'react'
import { Route, Routes, useLocation } from 'react-router-dom'
import { BackButton } from './components/BackButton'
import { Sidebar, type NavEntry } from './components/Sidebar'
import { TitleBar } from './components/TitleBar'
import DashboardPage from './pages/DashboardPage'
import ReviewQueuePage from './pages/ReviewQueuePage'
import AuditQueryPage from './pages/AuditQueryPage'
import ExceptionPage from './pages/ExceptionPage'
import GroupsPage from './pages/GroupsPage'
import TokensPage from './pages/TokensPage'
import HealthPage from './pages/HealthPage'
import ToolsPage from './pages/ToolsPage'
import UpstreamsPage from './pages/UpstreamsPage'
import CapabilityDossierPage, { CapabilityDossier } from './pages/CapabilityDossierPage'
import { AdminViewContext, VIEW_TITLE, useAdminViewStack } from './viewStack'

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
  { to: '/review', label: '审核队列', icon: 'review', title: '审核队列' },
  { to: '/tools', label: '工具', icon: 'tools', title: '工具台账' },
  { to: '/upstreams', label: 'MCP 接入', icon: 'link', title: 'MCP 接入' },
  { to: '/groups', label: '组维护', icon: 'groups', title: '组维护' },
  { to: '/tokens', label: '访问令牌', icon: 'key', title: '访问令牌' },
  { to: '/audit', label: '审计查询', icon: 'audit', title: '审计查询' },
  { to: '/exceptions', label: '例外与可见性', icon: 'exception', title: '例外与可见性' },
  { to: '/health', label: '健康', icon: 'health', title: '健康检查' },
]

const COLLAPSED_KEY = 'jumcp-admin-nav-collapsed'

export default function App() {
  const location = useLocation()
  const [collapsed, setCollapsed] = useState(
    () => window.localStorage.getItem(COLLAPSED_KEY) === '1',
  )

  useEffect(() => {
    window.localStorage.setItem(COLLAPSED_KEY, collapsed ? '1' : '0')
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
          onToggleCollapsed={() => setCollapsed((value) => !value)}
          onNavigate={clear}
        />
        <div className="shell-main">
          <TitleBar title={title} />
          <main className="content" id="main-content" tabIndex={-1}>
            {view === null ? (
              <Routes>
                <Route path="/" element={<DashboardPage />} />
                <Route path="/review" element={<ReviewQueuePage />} />
                <Route path="/tools" element={<ToolsPage />} />
                <Route path="/upstreams" element={<UpstreamsPage />} />
                <Route path="/capabilities/:id" element={<CapabilityDossierPage />} />
                <Route path="/groups" element={<GroupsPage />} />
                <Route path="/tokens" element={<TokensPage />} />
                <Route path="/audit" element={<AuditQueryPage />} />
                <Route path="/exceptions" element={<ExceptionPage />} />
                <Route path="/health" element={<HealthPage />} />
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
              <AuditQueryPage
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
          </main>
        </div>
      </div>
    </AdminViewContext.Provider>
  )
}
