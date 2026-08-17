import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { useVirtualizer } from "@tanstack/react-virtual";
import { useStore } from "../../state/store";
import { api, FileRow } from "../../lib/ipc";
import { thumbUrl, THUMB_GRID } from "../../lib/media";
import { useInViewThumb } from "../../lib/useInViewThumb";
import { fmtDuration, fmtSize } from "../../lib/format";

// Cell: thumb + 1 dòng tên. CELL_W là bề rộng TỐI THIỂU để tính số cột;
// bề rộng thật của ô co giãn chia đều phần dư → lưới luôn khít mép phải,
// không còn dải đen thừa (kiểu Google Photos).
const CELL_W = 168;
const THUMB_H = 152;
const CELL_H = THUMB_H + 34;
const GAP = 8;

function Cell({
  row,
  index,
  cellW,
}: {
  row: FileRow | undefined;
  index: number;
  cellW: number;
}) {
  const openLightbox = useStore((s) => s.openLightbox);
  const toggleSelect = useStore((s) => s.toggleSelect);
  const picked = useStore((s) => (row ? s.selected.has(row.id) : false));
  const anyPicked = useStore((s) => s.selected.size > 0);
  const [failed, setFailed] = useState(false);
  const { ref: thumbRef, wanted } = useInViewThumb(row?.id);
  useEffect(() => {
    setFailed(false);
  }, [row?.id]);

  if (!row) {
    return (
      <div
        ref={thumbRef}
        className="animate-pulse rounded bg-neutral-900"
        style={{ width: cellW, height: CELL_H }}
      />
    );
  }

  const icon =
    row.status === 2 ? "☁️" : failed ? (row.kind === 1 ? "🎬" : "🖼️") : null;

  const fileId = row.id;
  return (
    <button
      ref={thumbRef}
      className="group flex flex-col overflow-hidden rounded text-left outline-none focus-visible:ring-1 focus-visible:ring-neutral-400"
      style={{ width: cellW }}
      title={`${row.name}\n${row.dir}\n${fmtSize(row.size)}`}
      // Bấm thường vẫn MỞ ẢNH như trước — chọn nằm ở ô tick góc trái, hoặc
      // Ctrl/Shift+bấm. Đổi nghĩa cú bấm thường là phá thói quen xem ảnh.
      onClick={(e) => {
        if (e.ctrlKey || e.metaKey || e.shiftKey) {
          e.preventDefault();
          void toggleSelect(index, fileId, e.shiftKey);
        } else {
          openLightbox(index);
        }
      }}
    >
      <div
        className={`relative flex w-full items-center justify-center overflow-hidden rounded bg-neutral-900 ${
          picked ? "ring-2 ring-emerald-500" : ""
        }`}
        style={{ height: THUMB_H }}
      >
        {/* Hiện khi hover, khi chính nó đang được chọn, hoặc khi đang có
            selection — lúc đó user đang gom file nên cần thấy mọi ô tick */}
        <span
          role="checkbox"
          aria-checked={picked}
          tabIndex={-1}
          className={`absolute left-1 top-1 z-10 flex h-5 w-5 items-center justify-center rounded border text-[11px] leading-none transition-opacity ${
            picked
              ? "border-emerald-400 bg-emerald-500 text-neutral-950"
              : "border-neutral-400 bg-black/50 text-transparent"
          } ${picked || anyPicked ? "opacity-100" : "opacity-0 group-hover:opacity-100"}`}
          onClick={(e) => {
            e.stopPropagation();
            e.preventDefault();
            void toggleSelect(index, fileId, e.shiftKey);
          }}
        >
          ✓
        </span>
        {icon ? (
          <span className="text-3xl opacity-60">{icon}</span>
        ) : wanted ? (
          <img
            src={thumbUrl(row.id, THUMB_GRID, row.mtime)}
            alt=""
            loading="lazy"
            decoding="async"
            draggable={false}
            className="h-full w-full object-cover transition-transform duration-100 group-hover:scale-[1.03]"
            onError={() => setFailed(true)}
          />
        ) : null}
        {row.kind === 1 && row.durationMs != null && (
          <span className="absolute bottom-1 right-1 rounded bg-black/70 px-1 text-[10px] tabular-nums text-neutral-200">
            {fmtDuration(row.durationMs)}
          </span>
        )}
        {row.kind === 1 && row.durationMs == null && !icon && (
          <span className="absolute bottom-1 right-1 rounded bg-black/70 px-1 text-[10px] text-neutral-200">
            ▶
          </span>
        )}
        {row.isLive && row.kind === 0 && (
          <span className="absolute left-1 top-1 rounded bg-black/70 px-1 text-[9px] font-semibold tracking-wide text-amber-300">
            ◉ LIVE
          </span>
        )}
      </div>
      <span className="mt-1 w-full truncate text-xs text-neutral-400 group-hover:text-neutral-200">
        {row.name}
      </span>
    </button>
  );
}

/** Nhãn Tháng/Năm của hàng trên cùng đang nhìn thấy — nổi đè lên lưới.
 *
 * KHÔNG phải header chèn giữa lưới: lưới ảo hoá chỉ nạp cửa sổ đang xem nên
 * không biết ngày của những file chưa nạp, mà cắt mốc tháng thì cần biết ngày
 * của TẤT CẢ. Làm đúng kiểu đó phải có thêm truy vấn đếm file theo tháng rồi
 * dựng layout cao thấp khác nhau — đắt hơn hẳn. Nhãn nổi cho đúng thông tin
 * "đang ở quãng thời gian nào" mà không đụng gì tới layout.
 */
function DateBadge({ row, field }: { row: FileRow | undefined; field: string }) {
  const { i18n } = useTranslation();
  // Phải bám ĐÚNG trường đang sắp xếp, nếu không nhãn nói một đằng lưới xếp
  // một nẻo. taken_at là wall-clock đóng gói khung UTC nên đọc bằng getUTC*;
  // mtime là instant thật, đọc theo giờ máy.
  const byTaken = field !== "mtime";
  const ms = byTaken ? (row?.takenAt ?? row?.mtime) : row?.mtime;
  if (ms == null) return null;
  const d = new Date(ms);
  const [y, m] = byTaken
    ? [d.getUTCFullYear(), d.getUTCMonth()]
    : [d.getFullYear(), d.getMonth()];
  const label = new Intl.DateTimeFormat(i18n.language, {
    year: "numeric",
    month: "long",
    timeZone: "UTC",
  }).format(Date.UTC(y, m, 1));
  return (
    <div className="pointer-events-none sticky top-0 z-10 -mx-2 -mt-2 mb-1 px-3 py-1">
      <span className="rounded bg-neutral-900/90 px-2 py-0.5 text-xs font-medium text-neutral-300 shadow ring-1 ring-neutral-700">
        {label}
      </span>
    </div>
  );
}

export function FileGrid() {
  const total = useStore((s) => s.total);
  const rows = useStore((s) => s.rows);
  const queryId = useStore((s) => s.queryId);
  const filterEpoch = useStore((s) => s.filterEpoch);
  const sort = useStore((s) => s.filter.sort);
  const dateField = useStore((s) => s.filter.dateField);
  const parentRef = useRef<HTMLDivElement>(null);
  const [layout, setLayout] = useState({ cols: 4, cellW: CELL_W });
  const { cols, cellW } = layout;

  // Số cột theo bề rộng thật của container (ResizeObserver, không đoán window);
  // phần dư chia đều vào bề rộng ô để hàng luôn khít mép phải.
  useLayoutEffect(() => {
    const el = parentRef.current;
    if (!el) return;
    const update = () => {
      const w = el.clientWidth - 16; // padding x
      const cols = Math.max(2, Math.floor((w + GAP) / (CELL_W + GAP)));
      const cellW = Math.max(CELL_W, Math.floor((w - (cols - 1) * GAP) / cols));
      setLayout({ cols, cellW });
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
    // Filter đổi → thumb đang xếp hàng phía Rust là của query cũ, xả đi
    void api.clearThumbQueue();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [filterEpoch]);

  // Rời chế độ grid (sang list/tab khác): request cũ đã bị webview hủy —
  // xả nốt hàng đợi phía Rust để không giành I/O với job nền.
  useEffect(() => {
    return () => {
      void api.clearThumbQueue();
    };
  }, []);

  useEffect(() => {
    if (firstRow >= 0) {
      useStore.getState().ensureRange(firstRow * cols, (lastRow + 1) * cols - 1);
    }
  }, [firstRow, lastRow, cols, queryId]);

  // Sắp theo tên/dung lượng thì thứ tự chẳng liên quan gì tới thời gian —
  // nhãn ngày lúc đó là thông tin sai lệch, giấu đi.
  const sortedByDate = !["name", "size_desc", "size_asc"].includes(sort ?? "");

  return (
    <div ref={parentRef} className="min-h-0 flex-1 overflow-y-auto px-2 py-2">
      {sortedByDate && (
        <DateBadge
          row={firstRow >= 0 ? rows.get(firstRow * cols) : undefined}
          field={dateField ?? "taken"}
        />
      )}
      <div style={{ height: virtualizer.getTotalSize(), position: "relative" }}>
        {items.map((vi) => (
          <div
            key={vi.key}
            className="absolute left-0 top-0 flex w-full"
            style={{ gap: GAP, transform: `translateY(${vi.start}px)` }}
          >
            {Array.from({ length: cols }, (_, c) => {
              const idx = vi.index * cols + c;
              if (idx >= total) return <span key={c} style={{ width: cellW }} />;
              return <Cell key={c} row={rows.get(idx)} index={idx} cellW={cellW} />;
            })}
          </div>
        ))}
      </div>
    </div>
  );
}
