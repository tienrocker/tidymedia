import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import { useStore } from "../../state/store";
import { FileRow } from "../../lib/ipc";
import { thumbUrl, THUMB_GRID } from "../../lib/media";
import { fmtSize } from "../../lib/format";

// Cell: thumb vuông + 1 dòng tên. Cột tự co theo bề rộng container.
const CELL_W = 168;
const THUMB_H = 152;
const CELL_H = THUMB_H + 34;
const GAP = 8;

function Cell({ row, index }: { row: FileRow | undefined; index: number }) {
  const openLightbox = useStore((s) => s.openLightbox);
  const [failed, setFailed] = useState(false);
  // Row thay đổi (trang khác load vào slot) → reset trạng thái lỗi ảnh
  useEffect(() => setFailed(false), [row?.id]);

  if (!row) {
    return (
      <div
        className="animate-pulse rounded bg-neutral-900"
        style={{ width: CELL_W, height: CELL_H }}
      />
    );
  }

  const icon =
    row.status === 2 ? "☁️" : row.kind === 1 ? "🎬" : failed ? "🖼️" : null;

  return (
    <button
      className="group flex flex-col overflow-hidden rounded text-left outline-none focus-visible:ring-1 focus-visible:ring-neutral-400"
      style={{ width: CELL_W }}
      title={`${row.name}\n${row.dir}\n${fmtSize(row.size)}`}
      onClick={() => openLightbox(index)}
    >
      <div
        className="flex w-full items-center justify-center overflow-hidden rounded bg-neutral-900"
        style={{ height: THUMB_H }}
      >
        {icon ? (
          <span className="text-3xl opacity-60">{icon}</span>
        ) : (
          <img
            src={thumbUrl(row.id, THUMB_GRID, row.mtime)}
            alt=""
            loading="lazy"
            decoding="async"
            draggable={false}
            className="h-full w-full object-cover transition-transform duration-100 group-hover:scale-[1.03]"
            onError={() => setFailed(true)}
          />
        )}
      </div>
      <span className="mt-1 w-full truncate text-xs text-neutral-400 group-hover:text-neutral-200">
        {row.name}
      </span>
    </button>
  );
}

export function FileGrid() {
  const total = useStore((s) => s.total);
  const rows = useStore((s) => s.rows);
  const queryId = useStore((s) => s.queryId);
  const filterEpoch = useStore((s) => s.filterEpoch);
  const parentRef = useRef<HTMLDivElement>(null);
  const [cols, setCols] = useState(4);

  // Số cột theo bề rộng thật của container (ResizeObserver, không đoán window)
  useLayoutEffect(() => {
    const el = parentRef.current;
    if (!el) return;
    const update = () => {
      const w = el.clientWidth - 16; // padding x
      setCols(Math.max(2, Math.floor((w + GAP) / (CELL_W + GAP))));
    };
    update();
    const ro = new ResizeObserver(update);
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  const rowCount = Math.ceil(total / cols);
  const virtualizer = useVirtualizer({
    count: rowCount,
    getScrollElement: () => parentRef.current,
    estimateSize: () => CELL_H + GAP,
    overscan: 4,
  });

  const items = virtualizer.getVirtualItems();
  const firstRow = items.length ? items[0].index : -1;
  const lastRow = items.length ? items[items.length - 1].index : -1;

  useEffect(() => {
    virtualizer.scrollToOffset(0);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [filterEpoch]);

  useEffect(() => {
    if (firstRow >= 0) {
      useStore.getState().ensureRange(firstRow * cols, (lastRow + 1) * cols - 1);
    }
  }, [firstRow, lastRow, cols, queryId]);

  return (
    <div ref={parentRef} className="min-h-0 flex-1 overflow-y-auto px-2 py-2">
      <div style={{ height: virtualizer.getTotalSize(), position: "relative" }}>
        {items.map((vi) => (
          <div
            key={vi.key}
            className="absolute left-0 top-0 flex w-full"
            style={{ gap: GAP, transform: `translateY(${vi.start}px)` }}
          >
            {Array.from({ length: cols }, (_, c) => {
              const idx = vi.index * cols + c;
              if (idx >= total) return <span key={c} style={{ width: CELL_W }} />;
              return <Cell key={c} row={rows.get(idx)} index={idx} />;
            })}
          </div>
        ))}
      </div>
    </div>
  );
}
