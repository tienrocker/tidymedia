import { open } from "@tauri-apps/plugin-dialog";
import { useTranslation } from "react-i18next";
import { useStore } from "../../state/store";
import { fmtCount } from "../../lib/format";

export function RootsPanel() {
  const { t } = useTranslation();
  const roots = useStore((s) => s.roots);

  const onAdd = async () => {
    const dir = await open({ directory: true, title: t("roots.pickTitle") });
    if (typeof dir === "string") {
      try {
        await useStore.getState().addRootAndScan(dir);
      } catch (e) {
        alert(String(e));
      }
    }
  };

  const onRemove = async (id: number, path: string) => {
    if (confirm(t("roots.removeConfirm", { path }))) {
      await useStore.getState().removeRoot(id);
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
          className="rounded bg-neutral-800 px-2 py-0.5 text-xs text-neutral-200 hover:bg-neutral-700"
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
              {t("roots.files", { n: fmtCount(r.fileCount) })}
            </div>
          </div>
          <button
            title={t("roots.rescan")}
            className="hidden rounded px-1 text-xs text-neutral-400 hover:bg-neutral-800 group-hover:block"
            onClick={() => useStore.getState().scanRoot(r.id)}
          >
            ⟳
          </button>
          <button
            title={t("roots.remove")}
            className="hidden rounded px-1 text-xs text-red-400 hover:bg-neutral-800 group-hover:block"
            onClick={() => onRemove(r.id, r.path)}
          >
            ✕
          </button>
        </div>
      ))}
    </div>
  );
}
