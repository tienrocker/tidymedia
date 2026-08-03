import { useEffect, useRef } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import { useStore } from "../../state/store";
import { fmtDate, fmtSize } from "../../lib/format";

const ROW_H = 30;
const GRID_COLS =
  "grid grid-cols-[28px_minmax(180px,2fr)_minmax(180px,3fr)_90px_130px] items-center gap-2 px-3";

export function FileList() {
  const total = useStore((s) => s.total);
  const rows = useStore((s) => s.rows);
  const queryId = useStore((s) => s.queryId);
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

  useEffect(() => {
    if (firstIdx >= 0) {
      useStore.getState().ensureRange(firstIdx, lastIdx);
    }
  }, [firstIdx, lastIdx, queryId]);

  // Query mới → cuộn về đầu
  useEffect(() => {
    virtualizer.scrollToOffset(0);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [queryId]);

  return (
    <div className="min-h-0 flex-1">
      <div className={`${GRID_COLS} border-b border-neutral-800 py-1 text-xs font-semibold uppercase tracking-wide text-neutral-500`}>
        <span />
        <span>Tên</span>
        <span>Thư mục</span>
        <span className="text-right">Size</span>
        <span className="text-right">Ngày sửa</span>
      </div>
      <div ref={parentRef} className="h-[calc(100%-25px)] overflow-y-auto">
        <div style={{ height: virtualizer.getTotalSize(), position: "relative" }}>
          {items.map((vi) => {
            const row = rows.get(vi.index);
            return (
              <div
                key={vi.key}
                className={`${GRID_COLS} absolute left-0 top-0 w-full border-b border-neutral-900 hover:bg-neutral-900`}
                style={{ height: ROW_H, transform: `translateY(${vi.start}px)` }}
              >
                {row ? (
                  <>
                    <span className="text-center">{row.kind === 1 ? "🎬" : "🖼️"}</span>
                    <span className="truncate text-neutral-200" title={row.name}>
                      {row.name}
                    </span>
                    <span className="truncate text-neutral-500" title={row.dir}>
                      {row.dir}
                    </span>
                    <span className="text-right tabular-nums text-neutral-400">
                      {fmtSize(row.size)}
                    </span>
                    <span className="text-right tabular-nums text-neutral-400">
                      {fmtDate(row.mtime)}
                    </span>
                  </>
                ) : (
                  <>
                    <span />
                    <span className="h-3 w-40 animate-pulse rounded bg-neutral-900" />
                    <span className="h-3 w-56 animate-pulse rounded bg-neutral-900" />
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
