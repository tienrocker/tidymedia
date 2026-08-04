import { open } from "@tauri-apps/plugin-dialog";
import { useTranslation } from "react-i18next";
import { runSafe, useStore } from "../../state/store";
import { fmtCount } from "../../lib/format";

export function RootsPanel() {
  const { t } = useTranslation();
  const roots = useStore((s) => s.roots);
  const activeJobs = useStore((s) => s.activeJobs);
  const scanning = activeJobs.size > 0;

  const onAdd = () => {
    runSafe(async () => {
      const dir = await open({ directory: true, title: t("roots.pickTitle") });
      if (typeof dir === "string") {
        await useStore.getState().addRootAndScan(dir);
      }
    });
  };

  const onRemove = (id: number, path: string) => {
    if (confirm(t("roots.removeConfirm", { path }))) {
      runSafe(() => useStore.getState().removeRoot(id));
    }
  };

  return (
    <div className="flex flex-col gap-1 px-2">
      <div className="flex items-center justify-between px-1 py-1">
        <span className="text-xs font-semibold uppercase tracking-wide text-neutral-500">
          {t("roots.title")}
        </span>
        <button
          onClick={onAdd}
          disabled={scanning}
          className="rounded bg-neutral-800 px-2 py-0.5 text-xs text-neutral-200 hover:bg-neutral-700 disabled:opacity-30"
        >
          {t("roots.add")}
        </button>
      </div>
      {roots.length === 0 && (
        <div className="px-1 py-2 text-xs text-neutral-600">{t("roots.empty")}</div>
      )}
      {roots.map((r) => (
        <div
          key={r.id}
          className="group flex items-center gap-2 rounded px-1 py-1 hover:bg-neutral-900"
        >
          <div className="min-w-0 flex-1">
            <div className="truncate text-neutral-300" title={r.path}>
              {r.path}
            </div>
            <div className="text-xs text-neutral-600">
              {t("roots.files", { count: r.fileCount, formatted: fmtCount(r.fileCount) })}
            </div>
          </div>
          <button
            title={t("roots.rescan")}
            disabled={scanning}
            className="hidden rounded px-1 text-xs text-neutral-400 hover:bg-neutral-800 disabled:opacity-30 group-hover:block"
            onClick={() => runSafe(() => useStore.getState().scanRoot(r.id))}
          >
            ⟳
          </button>
          <button
            title={t("roots.remove")}
            disabled={scanning}
            className="hidden rounded px-1 text-xs text-red-400 hover:bg-neutral-800 disabled:opacity-30 group-hover:block"
            onClick={() => onRemove(r.id, r.path)}
          >
            ✕
          </button>
        </div>
      ))}
    </div>
  );
}
