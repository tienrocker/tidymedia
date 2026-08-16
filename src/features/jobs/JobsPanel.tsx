import { useTranslation } from "react-i18next";
import { runSafe, useStore } from "../../state/store";
import { fmtCount } from "../../lib/format";
import { errText } from "../../lib/errors";

/** Message do backend gửi là CODE ổn định; chỉ dịch cái mình biết, còn lại
 *  hiện nguyên văn (vd "warmed +120 thumbs"). */
function jobNote(message: string | null | undefined, t: (k: string) => string): string {
  if (!message) return "";
  if (message === "paused") return t("jobs.paused");
  if (message === "quick" || message === "verify" || message === "warm") return "";
  if (message === "trash" || message === "hash") return "";
  return message;
}

export function JobsPanel() {
  const { t } = useTranslation();
  const active = useStore((s) => s.activeJobs);
  const recent = useStore((s) => s.recentJobs);
  const kindLabel = (kind: string) =>
    t(`jobs.kind.${kind}`, { defaultValue: kind }) as string;

  const lastFinished = recent.find((j) => j.state !== "running");

  return (
    <div className="border-t border-neutral-800 px-2 py-2">
      <div className="px-1 pb-1 text-xs font-semibold uppercase tracking-wide text-neutral-500">
        {t("jobs.title")}
      </div>
      {active.size === 0 && (
        <div className="px-1 text-xs text-neutral-600">
          {lastFinished ? (
            <span className={lastFinished.state === "failed" ? "text-red-400" : ""}>
              {kindLabel(lastFinished.kind)}: {lastFinished.state}
              {lastFinished.error
                ? ` - ${errText(lastFinished.error)}`
                : lastFinished.message
                  ? ` - ${lastFinished.message}`
                  : ""}
            </span>
          ) : (
            t("jobs.none")
          )}
        </div>
      )}
      {[...active.values()].map((j) => {
        const note = jobNote(j.message, t);
        const paused = j.message === "paused";
        const pct =
          j.total != null && Number(j.total) > 0
            ? Math.min(100, (Number(j.done) / Number(j.total)) * 100)
            : null;
        return (
          <div key={j.jobId} className="px-1 py-1">
            <div className="flex items-center gap-2">
              <span className="min-w-0 flex-1 truncate text-xs text-neutral-300">
                {kindLabel(j.kind)}
                {note && <span className="text-neutral-500"> · {note}</span>}
              </span>
              <span className="shrink-0 text-[11px] tabular-nums text-neutral-500">
                {fmtCount(Number(j.done))}
                {j.total != null ? ` / ${fmtCount(Number(j.total))}` : ""}
              </span>
              <span
                className={`h-2 w-2 shrink-0 rounded-full ${
                  paused ? "bg-neutral-600" : "animate-pulse bg-emerald-500"
                }`}
              />
              <button
                className="shrink-0 rounded px-1 text-xs text-red-400 hover:bg-neutral-800"
                onClick={() => runSafe(() => useStore.getState().cancelJob(j.jobId))}
              >
                {t("jobs.stop")}
              </button>
            </div>
            <div className="mt-1 h-0.5 w-full overflow-hidden rounded bg-neutral-800">
              <div
                className={`h-full ${paused ? "bg-neutral-600" : "bg-emerald-600"}`}
                style={{ width: pct != null ? `${pct}%` : "100%" }}
              />
            </div>
          </div>
        );
      })}
    </div>
  );
}
