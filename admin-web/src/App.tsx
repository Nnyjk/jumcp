import { useEffect, useState } from 'react'
import { Route, Routes, useLocation } from 'react-router-dom'
import { Sidebar, type NavEntry } from './components/Sidebar'
import { TitleBar, type PageMode } from './components/TitleBar'
import ReviewQueuePage from './pages/ReviewQueuePage'
import AuditQueryPage from './pages/AuditQueryPage'
import ExceptionPage from './pages/ExceptionPage'
import HealthPage from './pages/HealthPage'

interface PageMeta extends NavEntry {
  title: string
  subtitle: string
  mode: PageMode
}

/** 首页等价于审核队列；路由元数据同时驱动侧栏与 TitleBar，避免标题两处维护。 */
const REVIEW_QUEUE: PageMeta = {
  to: '/review',
  label: '审核队列',
  icon: 'review',
  title: '审核队列',
  subtitle: 'draft / review 状态的能力版本等待审批；提交与批准必须为不同主体（四眼原则）。',
  mode: '未实现',
}

const NAV_ITEMS: PageMeta[] = [
  REVIEW_QUEUE,
  {
    to: '/audit',
    label: '审计查询',
    icon: 'audit',
    title: '审计查询',
    subtitle: '按 §4.4.3 审计 envelope 查询追加日志；只读视图，不可导出后回写状态。',
    mode: '只读',
  },
  {
    to: '/exceptions',
    label: '例外处理',
    icon: 'exception',
    title: '例外处理',
    subtitle: 'discoverable / callable 双布尔的只读展示；仅改双布尔不触发重新审核，但必须记 admin_change。',
    mode: '只读',
  },
  {
    to: '/health',
    label: '健康',
    icon: 'health',
    title: '健康检查',
    subtitle: '确认管理台与后端连通性及审计链完整性；本页不提供任何执行入口。',
    mode: '只读',
  },
]

const COLLAPSED_KEY = 'hjzl-admin-nav-collapsed'

export default function App() {
  const location = useLocation()
  const [collapsed, setCollapsed] = useState(
    () => window.localStorage.getItem(COLLAPSED_KEY) === '1',
  )

  useEffect(() => {
    window.localStorage.setItem(COLLAPSED_KEY, collapsed ? '1' : '0')
  }, [collapsed])

  const active = NAV_ITEMS.find((item) => location.pathname.startsWith(item.to)) ?? REVIEW_QUEUE

  return (
    <div className="shell" data-collapsed={collapsed}>
      <a className="skip-link" href="#main-content">
        跳到主要内容
      </a>
      <Sidebar
        items={NAV_ITEMS}
        collapsed={collapsed}
        onToggleCollapsed={() => setCollapsed((value) => !value)}
      />
      <div className="shell-main">
        <TitleBar title={active.title} subtitle={active.subtitle} mode={active.mode} />
        <main className="content" id="main-content">
          <Routes>
            <Route path="/" element={<ReviewQueuePage />} />
            <Route path="/review" element={<ReviewQueuePage />} />
            <Route path="/audit" element={<AuditQueryPage />} />
            <Route path="/exceptions" element={<ExceptionPage />} />
            <Route path="/health" element={<HealthPage />} />
            <Route path="*" element={<ReviewQueuePage />} />
          </Routes>
        </main>
      </div>
    </div>
  )
}
