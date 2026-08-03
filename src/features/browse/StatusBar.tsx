import { useTranslation } from "react-i18next";
import { useStore } from "../../state/store";
import { fmtCount } from "../../lib/format";

export function StatusBar() {
  const { t } = useTranslation();
  const total = useStore((s) => s.total);
  const queryMs = useStore((s) => s.queryMs);
  const querying = useStore((s) => s.querying);

  return (
    <div className="flex items-center gap-4 border-t border-neutral-800 bg-neutral-950 px-3 py-1 text-xs text-neutral-500">
      <span>{t("status.files", { count: total, formatted: fmtCount(total) })}</span>
      {queryMs != null && !querying && (
        <span>{t("status.query", { ms: queryMs.toFixed(0) })}</span>
      )}
      {querying && <span className="animate-pulse">…</span>}
    </div>
  );
}
