/** 标签与路径的列表框：更改数据目录的确认框与启动提示中使用 */
export default function PathBox({ rows }: { rows: [string, string][] }) {
  return (
    <div data-selectable-text className="space-y-1.5 rounded-xl bg-black/[0.03] px-3 py-2.5 text-xs leading-relaxed dark:bg-white/[0.04]">
      {rows.map(([label, path]) => (
        <div key={label} className="flex gap-3">
          <span className="shrink-0 text-gray-400 dark:text-gray-500">{label}</span>
          <span className="min-w-0 break-all text-gray-600 dark:text-gray-300">{path}</span>
        </div>
      ))}
    </div>
  )
}
