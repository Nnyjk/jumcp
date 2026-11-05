import { NavLink, Route, Routes } from 'react-router-dom'
import ReviewQueuePage from './pages/ReviewQueuePage'
import AuditQueryPage from './pages/AuditQueryPage'
import ExceptionPage from './pages/ExceptionPage'
import HealthPage from './pages/HealthPage'

const NAV_ITEMS = [
  { to: '/review', label: '审核队列' },
  { to: '/audit', label: '审计查询' },
  { to: '/exceptions', label: '例外处理' },
  { to: '/health', label: '健康' },
]

export default function App() {
  return (
    <div className="app">
      <header className="app-header">
        <div className="app-title">
          <h1>HJZL MCP 管理台</h1>
          <span className="badge badge-skeleton">只读联调 · 写操作未接通</span>
        </div>
        <nav className="app-nav">
          {NAV_ITEMS.map((item) => (
            <NavLink
              key={item.to}
              to={item.to}
              className={({ isActive }) => (isActive ? 'nav-link nav-link-active' : 'nav-link')}
            >
              {item.label}
            </NavLink>
          ))}
        </nav>
      </header>
      <main className="app-main">
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
  )
}
