import { NavLink } from 'react-router-dom'
import { gsap } from 'gsap'
import { Icon, type IconName } from './Icon'
import { MOTION_EASE, useGsapScope } from '../motion/useGsap'

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
  username,
  onToggleCollapsed,
  onNavigate,
  onSignOut,
}: {
  items: NavEntry[]
  collapsed: boolean
  /** 当前登录账户名（来自会话 actor.id）：只读展示，不代表可切换身份。 */
  username: string
  onToggleCollapsed: () => void
  /** 点击导航即离开操作视图，回到该路由的页面。 */
  onNavigate: () => void
  /** 吊销会话后回登录页；由 App 注入，侧栏不直接依赖 auth 模块。 */
  onSignOut: () => void
}) {
  const collapseLabel = collapsed ? '展开导航' : '折叠导航'
  const sidebarRef = useGsapScope<HTMLElement>((scope) => {
    // 折叠/展开时图标与文字只做透明度/位移过渡；宽度由既有 grid transition 负责。
    gsap.from(scope.querySelectorAll('.nav-link'), {
      opacity: 0,
      x: -6,
      duration: 0.2,
      ease: MOTION_EASE,
      stagger: 0.018,
      clearProps: 'opacity,transform',
    })
    if (!collapsed) {
      gsap.from(scope.querySelectorAll('.brand-text, .nav-collapse span'), {
        opacity: 0,
        x: -4,
        duration: 0.18,
        ease: MOTION_EASE,
        stagger: 0.02,
        clearProps: 'opacity,transform',
      })
    }
  }, [collapsed])

  return (
    <aside className="sidebar" data-collapsed={collapsed} ref={sidebarRef}>
      <div className="sidebar-brand">
        <span className="brand-mark">
          <Icon name="logo" size={18} />
        </span>
        {!collapsed && (
          <span className="brand-text">
            <strong>jumcp</strong>
            <small>治理面 · 管理台</small>
          </span>
        )}
      </div>

      <nav className="nav" id="primary-nav" aria-label="主导航">
        {items.map((item) => (
          <NavLink
            key={item.to}
            to={item.to}
            end={item.to === '/'}
            data-label={item.label}
            className={({ isActive }) => (isActive ? 'nav-link nav-link-active' : 'nav-link')}
            onClick={onNavigate}
          >
            <span className="nav-icon">
              <Icon name={item.icon} size={20} />
            </span>
            {!collapsed && <span className="nav-label">{item.label}</span>}
          </NavLink>
        ))}
      </nav>

      <div className="sidebar-foot">
        <div className="nav-account" title={collapsed ? `当前账户 ${username}` : undefined}>
          <span className="nav-account-mark" aria-hidden="true">
            {username.slice(0, 1).toUpperCase()}
          </span>
          {!collapsed && (
            <span className="nav-account-text">
              <small>当前账户</small>
              <strong className="mono">{username}</strong>
            </span>
          )}
          {collapsed && <span className="sr-only">当前账户 {username}</span>}
        </div>
        <button
          type="button"
          className="nav-collapse"
          onClick={onSignOut}
          aria-label="退出登录"
          title="退出登录"
        >
          <Icon name="logout" size={20} />
          {!collapsed && <span>退出登录</span>}
        </button>
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
      </div>
    </aside>
  )
}
