/**
 * 操作视图的返回入口：视图不写 URL，返回按钮是回到上一页（上一视图或路由页面）的唯一入口，
 * 视图态不进 history：浏览器后退会直接离开当前路由，而不是退出视图，所以本按钮是唯一出口。
 * 标签要写清具体去向（如「返回工具台账」），不能只写「返回」。
 */
export function BackButton({ label, onClick }: { label: string; onClick: () => void }) {
  return (
    <div className="view-back">
      <button type="button" className="btn btn-subtle btn-sm" onClick={onClick}>
        ← {label}
      </button>
    </div>
  )
}
