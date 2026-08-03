import { useTranslation } from "react-i18next";
import { useStore } from "../../state/store";
import { fmtCount } from "../../lib/format";

export function JobsPanel() {
  const { t } = useTranslation();
  const active = useStore((s) => s.activeJobs);
  const recent = useStore((s) => s.recentJobs);

  const lastFinished = recent.find((j) => j.state !== "running");

  return (
    <div className="border-t border-neutral-800 px-2 py-2">
      <div className="px-1 pb-1 text-xs font-semibold uppercase tracking-wide text-neutral-500">
        {t("jobs.title")}
      </div>
      {active.size === 0 && (
        <div className="px-1 text-xs text-neutral-600">
          {lastFinished
            ? `${lastFinished.kind} #${lastFinished.id}: ${lastFinished.state}${
                lastFinished.message ? ` — ${lastFinished.message}` : ""
              }`
            : t("jobs.none")}
        </div>
      )}
      {[...active.values()].map((j) => (
        <div key={j.jobId} className="flex items-center gap-2 px-1 py-0.5">
          <span className="flex-1 truncate text-xs text-neutral-300">
            {j.kind} #{j.jobId} — {fmtCount(j.done)}
            {j.total != null ? ` / ${fmtCount(j.total)}` : ""}
          </span>
          <span className="h-2 w-2 animate-pulse rounded-full bg-emerald-500" />
          <button
            className="rounded px-1 text-xs text-red-400 hover:bg-neutral-800"
            onClick={() => useStore.getState().cancelJob(j.jobId)}
          >
            {t("jobs.stop")}
          </button>
        </div>
      ))}
    </div>
  );
}
