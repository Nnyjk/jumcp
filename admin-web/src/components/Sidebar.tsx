import { NavLink } from 'react-router-dom'
import { Icon, type IconName } from './Icon'

export interface NavEntry {
  to: string
  label: string
  icon: IconName
}

/**
 * 左侧 NavigationView：260px 展开 / 48px 仅图标折叠，选中态为 3px 强调色竖条 + 8px 圆角高亮。
 * 折叠时用 CSS tooltip 显示名称，键盘聚焦同样触发。
 */
export function Sidebar({
  items,
  collapsed,
  onToggleCollapsed,
}: {
  items: NavEntry[]
  collapsed: boolean
  onToggleCollapsed: () => void
}) {
  const collapseLabel = collapsed ? '展开导航' : '折叠导航'

  return (
    <aside className="sidebar" data-collapsed={collapsed}>
      <div className="sidebar-brand">
        <span className="brand-mark">
          <Icon name="logo" size={18} />
        </span>
        {!collapsed && (
          <span className="brand-text">
            <strong>HJZL MCP</strong>
            <small>治理面 · 管理台</small>
          </span>
        )}
      </div>

      <nav className="nav" id="primary-nav" aria-label="主导航">
        {items.map((item) => (
          <NavLink
            key={item.to}
            to={item.to}
            data-label={item.label}
            className={({ isActive }) => (isActive ? 'nav-link nav-link-active' : 'nav-link')}
          >
            <span className="nav-icon">
              <Icon name={item.icon} size={20} />
            </span>
            {!collapsed && <span className="nav-label">{item.label}</span>}
          </NavLink>
        ))}
      </nav>

      <div className="sidebar-foot">
        <button
          type="button"
          className="nav-collapse"
          onClick={onToggleCollapsed}
          aria-controls="primary-nav"
          aria-expanded={!collapsed}
          aria-label={collapseLabel}
          title={collapseLabel}
        >
          <Icon name="sidebar" size={20} />
          {!collapsed && <span>{collapseLabel}</span>}
        </button>
        {!collapsed && (
          <p className="sidebar-note">只读视图：写操作未接通，变更须经 admin API 落审计。</p>
        )}
      </div>
    </aside>
  )
}
