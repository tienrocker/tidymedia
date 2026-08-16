import { useEffect, useRef } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import { useTranslation } from "react-i18next";
import { useStore } from "../../state/store";
import { fmtSize } from "../../lib/format";
import { fmtDateTime } from "../../lib/time";

const ROW_H = 30;
const GRID_COLS =
  "grid grid-cols-[28px_minmax(180px,2fr)_minmax(180px,3fr)_100px_90px_130px] items-center gap-2 px-3";

export function FileList() {
  const { t } = useTranslation();
  const total = useStore((s) => s.total);
  const rows = useStore((s) => s.rows);
  const queryId = useStore((s) => s.queryId);
  const filterEpoch = useStore((s) => s.filterEpoch);
  const tz = useStore((s) => s.timezone);
  const tzOffset = useStore((s) => s.tzOffsetMinutes);
  const parentRef = useRef<HTMLDivElement>(null);

  const virtualizer = useVirtualizer({
    count: total,
    getScrollElement: () => parentRef.current,
    estimateSize: () => ROW_H,
    overscan: 30,
  });

  const items = virtualizer.getVirtualItems();
  const firstIdx = items.length ? items[0].index : -1;
  const lastIdx = items.length ? items[items.length - 1].index : -1;

  // FILTER đổi → về đầu list. Re-query nền (index://changed khi scan) thì GIỮ scroll.
  useEffect(() => {
    virtualizer.scrollToOffset(0);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [filterEpoch]);

  useEffect(() => {
    if (firstIdx >= 0) {
      useStore.getState().ensureRange(firstIdx, lastIdx);
    }
  }, [firstIdx, lastIdx, queryId]);

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div
        className={`${GRID_COLS} shrink-0 border-b border-neutral-800 py-1 text-xs font-semibold uppercase tracking-wide text-neutral-500`}
      >
        <span />
        <span>{t("list.name")}</span>
        <span>{t("list.folder")}</span>
        <span className="text-right">{t("list.dims")}</span>
        <span className="text-right">{t("list.size")}</span>
        <span className="text-right">{t("list.modified")}</span>
      </div>
      <div ref={parentRef} className="min-h-0 flex-1 overflow-y-auto">
        <div style={{ height: virtualizer.getTotalSize(), position: "relative" }}>
          {items.map((vi) => {
            const row = rows.get(vi.index);
            return (
              <div
                key={vi.key}
                className={`${GRID_COLS} absolute left-0 top-0 w-full cursor-pointer border-b border-neutral-900 hover:bg-neutral-900`}
                style={{ height: ROW_H, transform: `translateY(${vi.start}px)` }}
                onClick={() => row && useStore.getState().openLightbox(vi.index)}
                onDoubleClick={() => row && useStore.getState().openLightbox(vi.index)}
              >
                {row ? (
                  <>
                    <span className="text-center">
                      {row.status === 2 ? "☁️" : row.kind === 1 ? "🎬" : "🖼️"}
                    </span>
                    <span className="truncate text-neutral-200" title={row.name}>
                      {row.name}
                    </span>
                    <span className="truncate text-neutral-500" title={row.dir}>
                      {row.dir}
                    </span>
                    <span className="text-right tabular-nums text-neutral-500">
                      {row.width != null && row.height != null
                        ? `${row.width}×${row.height}`
                        : ""}
                    </span>
                    <span className="text-right tabular-nums text-neutral-400">
                      {fmtSize(row.size)}
                    </span>
                    <span className="text-right tabular-nums text-neutral-400">
                      {fmtDateTime(row.mtime, tz, tzOffset)}
                    </span>
                  </>
                ) : (
                  <>
                    <span />
                    <span className="h-3 w-40 animate-pulse rounded bg-neutral-900" />
                    <span className="h-3 w-56 animate-pulse rounded bg-neutral-900" />
                    <span />
                    <span />
                    <span />
                  </>
                )}
              </div>
            );
          })}
        </div>
      </div>
    </div>
  );
}
