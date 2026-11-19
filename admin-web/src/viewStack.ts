import { createContext, useCallback, useContext, useMemo, useState } from 'react'
import { useLocation } from 'react-router-dom'

/** 能力档案分栏；与页面内的 tab 序列一一对应。 */
export type DossierTab = 'overview' | 'definition' | 'versions' | 'approval' | 'calls' | 'audit'

/**
 * content 区内的操作视图：操作按钮不写 URL，只在当前 content 区替换渲染。
 * 路由仍然只由侧栏决定，视图栈不参与路由。
 */
export type AdminView =
  | { kind: 'tools'; group?: string | undefined }
  | { kind: 'dossier'; id: string; tab?: DossierTab | undefined }
  | {
      kind: 'audit'
      tokenId?: string | undefined
      objectId?: string | undefined
      eventId?: string | undefined
    }

/** TitleBar 标题与「返回 X」按钮共用同一份视图名称。 */
export const VIEW_TITLE: Record<AdminView['kind'], string> = {
  tools: '工具台账',
  dossier: '能力档案',
  audit: '审计',
}

export interface AdminViewApi {
  /** 栈顶视图；null 表示当前 URL 对应的路由页面。 */
  view: AdminView | null
  /** 返回目标的名称（上一视图，栈底时是当前路由页面）。 */
  backLabel: string
  openTools: (options?: { group?: string }) => void
  openDossier: (options: { id: string; tab?: DossierTab }) => void
  openAudit: (options?: { tokenId?: string; objectId?: string; eventId?: string }) => void
  back: () => void
  /** 侧栏导航时清空视图（同 URL 再点一次也回到路由页面）。 */
  clear: () => void
}

const EMPTY_VIEWS: AdminView[] = []

export const AdminViewContext = createContext<AdminViewApi | null>(null)

export function useAdminView(): AdminViewApi {
  const value = useContext(AdminViewContext)
  if (value === null) throw new Error('useAdminView 必须在 App 提供的视图上下文内使用')
  return value
}

/**
 * 轻量视图栈：栈里只放 content 区要展示的操作视图。
 * 栈记录自己所属的 pathname，路由一变栈就自然失效，不需要额外 effect 清空。
 */
export function useAdminViewStack(routeTitle: string): AdminViewApi {
  const { pathname } = useLocation()
  const [stack, setStack] = useState<{ routeKey: string; views: AdminView[] }>({
    routeKey: pathname,
    views: EMPTY_VIEWS,
  })

  // 路由已变但栈还记录着上一个页面时，按空栈处理（渲染期派生，避免闪一帧旧视图）。
  const views = stack.routeKey === pathname ? stack.views : EMPTY_VIEWS

  const push = useCallback(
    (view: AdminView) => {
      setStack((prev) => ({
        routeKey: pathname,
        views: [...(prev.routeKey === pathname ? prev.views : EMPTY_VIEWS), view],
      }))
    },
    [pathname],
  )

  const openTools = useCallback(
    (options?: { group?: string }) => push({ kind: 'tools', group: options?.group }),
    [push],
  )

  const openDossier = useCallback(
    (options: { id: string; tab?: DossierTab }) =>
      push({ kind: 'dossier', id: options.id, tab: options.tab }),
    [push],
  )

  const openAudit = useCallback(
    (options?: { tokenId?: string; objectId?: string; eventId?: string }) =>
      push({
        kind: 'audit',
        tokenId: options?.tokenId,
        objectId: options?.objectId,
        eventId: options?.eventId,
      }),
    [push],
  )

  const back = useCallback(() => {
    setStack((prev) => ({
      routeKey: pathname,
      views: prev.routeKey === pathname ? prev.views.slice(0, -1) : EMPTY_VIEWS,
    }))
  }, [pathname])

  const clear = useCallback(() => setStack({ routeKey: pathname, views: EMPTY_VIEWS }), [pathname])

  const view = views.length === 0 ? null : (views[views.length - 1] ?? null)
  const previous = views.length > 1 ? views[views.length - 2] : undefined
  const backLabel = previous === undefined ? routeTitle : VIEW_TITLE[previous.kind]

  return useMemo(
    () => ({ view, backLabel, openTools, openDossier, openAudit, back, clear }),
    [view, backLabel, openTools, openDossier, openAudit, back, clear],
  )
}
