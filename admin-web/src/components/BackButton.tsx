/** 操作视图的返回入口：视图不写 URL，返回按钮是回到上一页（视图或路由页面）的唯一入口。 */
export function BackButton({ label, onClick }: { label: string; onClick: () => void }) {
  return (
    <div className="view-back">
      <button type="button" className="btn btn-subtle btn-sm" onClick={onClick}>
        ← {label}
      </button>
    </div>
  )
}
