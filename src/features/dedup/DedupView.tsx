import { useEffect, useRef, useState } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import { useTranslation } from "react-i18next";
import { groupCheckState, runSafe, useStore, DedupRule } from "../../state/store";
import { api, DupGroupRow, DupMemberRow } from "../../lib/ipc";
import { thumbUrl, THUMB_GRID, THUMB_PREVIEW } from "../../lib/media";
import { useInViewThumb } from "../../lib/useInViewThumb";
import { fmtCount, fmtSize } from "../../lib/format";
import { fmtDateTime } from "../../lib/time";

const GROUP_ROW_H = 64;

/** Checkbox 3 trạng thái dùng chung cho row nhóm + "chọn tất cả". */
function TriCheckbox({
  state,
  disabled,
  title,
  onChange,
}: {
  state: "none" | "partial" | "all";
  disabled?: boolean;
  title?: string;
  onChange: (checked: boolean, shift: boolean) => void;
}) {
  const ref = useRef<HTMLInputElement>(null);
  useEffect(() => {
    if (ref.current) ref.current.indeterminate = state === "partial";
  }, [state]);
  return (
    <input
      ref={ref}
      type="checkbox"
      checked={state === "all"}
      readOnly // state do store quyết định; onClick mới là nguồn hành động
      disabled={disabled}
      title={title}
      className="h-4 w-4 shrink-0 cursor-pointer accent-red-600 disabled:opacity-40"
      onClick={(e) => {
        // Click checkbox KHÔNG được lan ra row (row = mở nhóm để xem)
        e.stopPropagation();
        // Đang dở dang mà bấm tiếp = tick nốt cho đủ, không phải bỏ tick
        onChange(state !== "all", e.shiftKey);
      }}
    />
  );
}

interface Zoom {
  scale: number;
  tx: number;
  ty: number;
}
const ZOOM_RESET: Zoom = { scale: 1, tx: 0, ty: 0 };

/** 1 dòng nhóm: checkbox (đánh dấu xóa theo rule) + 3 ảnh mẫu theo viewport. */
function GroupRow({
  g,
  active,
  marked,
  top,
}: {
  g: DupGroupRow;
  active: boolean;
  marked: number;
  top: number;
}) {
  const { t } = useTranslation();
  const marking = useStore((s) => s.dupMarking);
  // Ảnh mẫu cũng phải theo viewport: list 4k nhóm mà mount là bắn request thì
  // HDD nghẽn y như lưới ảnh trước đây.
  const { ref, wanted } = useInViewThumb(g.id);
  const state = groupCheckState(marked, g.count);

  return (
    <div
      ref={ref}
      className={`absolute left-0 top-0 flex w-full items-center gap-2 border-b border-neutral-900 px-2 ${
        active ? "bg-neutral-800" : "hover:bg-neutral-900"
      }`}
      style={{ height: GROUP_ROW_H, transform: `translateY(${top}px)` }}
    >
      <TriCheckbox
        state={state}
        disabled={marking}
        title={t("dedup.markGroup")}
        onChange={(checked, shift) =>
          runSafe(() => useStore.getState().setGroupChecked(g.id, checked, shift))
        }
      />
      <button
        className="flex min-w-0 flex-1 items-center gap-2 py-1 text-left"
        onClick={(e) => {
          // Blur để keyboard-flow (Space/1-9) không bị nuốt bởi button
          // đang giữ focus sau cú click
          e.currentTarget.blur();
          runSafe(() => useStore.getState().openDupGroup(g.id));
        }}
      >
        <div className="flex shrink-0 -space-x-4">
          {g.samples.map(([id, mtime]) => (
            <div
              key={id}
              className="h-12 w-12 overflow-hidden rounded border border-neutral-700 bg-neutral-900"
            >
              {wanted && (
                <img
                  src={thumbUrl(id, THUMB_GRID, mtime)}
                  alt=""
                  decoding="async"
                  className="h-full w-full object-cover"
                  onError={(e) => {
                    (e.target as HTMLImageElement).style.visibility = "hidden";
                  }}
                />
              )}
            </div>
          ))}
        </div>
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-1 text-sm text-neutral-200">
            {t("dedup.copies", { n: g.count })}
            {marked > 0 && (
              <span className="rounded bg-red-950 px-1 text-[10px] font-semibold text-red-300">
                🗑 {marked}
              </span>
            )}
          </div>
          <div className="truncate text-xs text-neutral-500">
            {fmtSize(g.size)} · {t("dedup.wasted", { size: fmtSize(g.waste) })}
          </div>
        </div>
      </button>
    </div>
  );
}

/** Cột trái: danh sách nhóm trùng, lãng phí nhiều nhất trước (ảo hóa). */
function GroupList() {
  const { t } = useTranslation();
  const groups = useStore((s) => s.dupGroups);
  const activeId = useStore((s) => s.activeGroupId);
  const checked = useStore((s) => s.dupChecked);
  const parentRef = useRef<HTMLDivElement>(null);

  const virtualizer = useVirtualizer({
    count: groups?.length ?? 0,
    getScrollElement: () => parentRef.current,
    estimateSize: () => GROUP_ROW_H,
    overscan: 10,
  });

  if (groups == null) {
    return <div className="p-3 text-sm text-neutral-500">…</div>;
  }
  if (groups.length === 0) {
    return <div className="p-3 text-sm text-neutral-500">{t("dedup.empty")}</div>;
  }
  return (
    <div ref={parentRef} className="min-h-0 flex-1 overflow-y-auto">
      <div style={{ height: virtualizer.getTotalSize(), position: "relative" }}>
        {virtualizer.getVirtualItems().map((vi) => {
          const g: DupGroupRow = groups[vi.index];
          return (
            <GroupRow
              key={g.id}
              g={g}
              active={g.id === activeId}
              marked={checked.get(g.id)?.size ?? 0}
              top={vi.start}
            />
          );
        })}
      </div>
    </div>
  );
}

/** Badge nhỏ đánh dấu "giá trị tốt nhất trong nhóm". */
function Best({ show, label }: { show: boolean; label: string }) {
  if (!show) return null;
  return (
    <span className="ml-1 rounded bg-emerald-900/60 px-1 text-[9px] font-semibold text-emerald-300">
      ★ {label}
    </span>
  );
}

function MemberCard({
  m,
  groupId,
  zoom,
  onZoom,
  focused,
  best,
  tz,
  tzOffset,
}: {
  m: DupMemberRow;
  groupId: number;
  zoom: Zoom;
  onZoom: (z: Zoom) => void;
  focused: boolean;
  best: { res: number; size: number; oldest: number };
  tz: string;
  tzOffset: number;
}) {
  const { t } = useTranslation();
  const checked = useStore((s) => s.dupChecked.get(groupId)?.has(m.fileId) ?? false);
  const drag = useRef<{ x: number; y: number } | null>(null);
  const boxRef = useRef<HTMLDivElement>(null);

  const px = (m.width ?? 0) * (m.height ?? 0);
  const when = m.takenAt ?? m.mtime;

  const zoomAt = (clientX: number, clientY: number, factor: number) => {
    const rect = boxRef.current?.getBoundingClientRect();
    if (!rect) return;
    const scale = Math.min(8, Math.max(1, zoom.scale * factor));
    if (scale === 1) {
      onZoom(ZOOM_RESET);
      return;
    }
    const cx = clientX - rect.left - rect.width / 2;
    const cy = clientY - rect.top - rect.height / 2;
    const k = scale / zoom.scale;
    onZoom({ scale, tx: cx - (cx - zoom.tx) * k, ty: cy - (cy - zoom.ty) * k });
  };

  return (
    <div
      className={`flex min-w-0 flex-col overflow-hidden rounded border-2 ${
        checked
          ? "border-red-700"
          : focused
            ? "border-neutral-400"
            : "border-emerald-900/60"
      }`}
    >
      {/* Preview - zoom/pan ĐỒNG BỘ giữa mọi card trong nhóm */}
      <div
        ref={boxRef}
        className="relative h-56 shrink-0 cursor-crosshair overflow-hidden bg-black"
        onWheel={(e) => zoomAt(e.clientX, e.clientY, e.deltaY < 0 ? 1.3 : 0.77)}
        onDoubleClick={(e) =>
          zoom.scale === 1 ? zoomAt(e.clientX, e.clientY, 3) : onZoom(ZOOM_RESET)
        }
        onPointerDown={(e) => {
          if (zoom.scale > 1 && e.button === 0) {
            drag.current = { x: e.clientX, y: e.clientY };
            (e.target as HTMLElement).setPointerCapture(e.pointerId);
          }
        }}
        onPointerMove={(e) => {
          if (drag.current) {
            const dx = e.clientX - drag.current.x;
            const dy = e.clientY - drag.current.y;
            drag.current = { x: e.clientX, y: e.clientY };
            onZoom({ ...zoom, tx: zoom.tx + dx, ty: zoom.ty + dy });
          }
        }}
        onPointerUp={() => {
          drag.current = null;
        }}
      >
        <img
          src={thumbUrl(m.fileId, zoom.scale > 1 ? THUMB_PREVIEW : THUMB_GRID, m.mtime)}
          alt={m.name}
          draggable={false}
          className="h-full w-full select-none object-contain"
          style={{
            transform: `translate(${zoom.tx}px, ${zoom.ty}px) scale(${zoom.scale})`,
          }}
        />
        {checked && (
          <div className="pointer-events-none absolute inset-0 flex items-center justify-center bg-red-950/50">
            <span className="text-4xl">🗑</span>
          </div>
        )}
        {m.isLive && (
          <span className="absolute left-1 top-1 rounded bg-black/70 px-1 text-[9px] font-semibold text-amber-300">
            ◉ LIVE
          </span>
        )}
        <label
          className={`absolute right-1 top-1 flex cursor-pointer items-center gap-1 rounded px-2 py-0.5 text-xs font-semibold ${
            checked
              ? "bg-red-700 text-white"
              : "bg-black/70 text-neutral-300 hover:bg-neutral-700"
          }`}
          title={t("dedup.markDelete")}
        >
          <input
            type="checkbox"
            checked={checked}
            readOnly
            className="h-3.5 w-3.5 cursor-pointer accent-red-600"
            onClick={(e) => {
              e.stopPropagation();
              useStore.getState().toggleDupChecked(groupId, m.fileId);
            }}
          />
          {checked ? t("dedup.marked") : t("dedup.markDelete")}
        </label>
      </div>

      {/* Metadata so sánh - giá trị tốt nhất được badge */}
      <div className="space-y-0.5 bg-neutral-950 px-2 py-1.5 text-xs">
        <div className="truncate text-neutral-200" title={m.name}>
          {m.name}
        </div>
        <div className="text-neutral-400">
          {m.width != null ? `${m.width}×${m.height}` : "?"}
          <Best show={px > 0 && px === best.res} label={t("dedup.bestRes")} />
        </div>
        <div className="text-neutral-400">
          {fmtSize(m.size)}
          <Best show={m.size === best.size} label={t("dedup.bestSize")} />
        </div>
        <div className="text-neutral-400">
          {fmtDateTime(when, m.takenAt != null ? 0 : tz, tzOffset)}
          <Best show={when === best.oldest} label={t("dedup.oldest")} />
        </div>
        <div className="truncate text-neutral-600" title={m.dir}>
          {m.dir}
        </div>
        <button
          className="text-neutral-500 underline-offset-2 hover:text-neutral-300 hover:underline"
          onClick={() => runSafe(() => api.revealFile(m.fileId))}
        >
          {t("lightbox.reveal")}
        </button>
      </div>
    </div>
  );
}

function GroupCompare() {
  const { t } = useTranslation();
  const groupId = useStore((s) => s.activeGroupId);
  const members = useStore((s) => s.groupMembers);
  const tz = useStore((s) => s.timezone);
  const tzOffset = useStore((s) => s.tzOffsetMinutes);
  const [zoom, setZoom] = useState<Zoom>(ZOOM_RESET);
  const [focusIdx, setFocusIdx] = useState(0);

  // Đổi nhóm → reset zoom + focus, và xả hàng đợi thumb của nhóm cũ (webview
  // đã hủy request rồi, nhưng phía Rust vẫn còn xếp hàng giành ổ đĩa)
  useEffect(() => {
    setZoom(ZOOM_RESET);
    setFocusIdx(0);
    void api.clearThumbQueue();
  }, [groupId]);

  // Mirror focusIdx ra ref cho keyboard handler (deps rỗng): gọi action trong
  // updater của setState là impure - StrictMode double-invoke chạy toggle 2
  // lần (check rồi uncheck ngay) làm Space thành no-op trong dev.
  const focusIdxRef = useRef(focusIdx);
  focusIdxRef.current = focusIdx;

  // Keyboard-first: ←→ chuyển card, Space đánh dấu, 1-4 giữ bản đó, Enter nhóm kế
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const el = e.target as HTMLElement | null;
      if (
        el &&
        (el.tagName === "INPUT" ||
          el.tagName === "TEXTAREA" ||
          el.tagName === "SELECT")
      ) {
        return;
      }
      // BUTTON: chỉ nhường Enter/Space (kích hoạt native - chồng thêm hành
      // động của mình là double-fire); mũi tên + 1-9 vẫn phải chạy để flow
      // bàn phím không chết sau 1 cú click chuột.
      if (el?.tagName === "BUTTON" && (e.key === "Enter" || e.key === " ")) {
        return;
      }
      const st = useStore.getState();
      if (st.appMode !== "dedup" || st.activeGroupId == null) return;
      const n = st.groupMembers.length;
      if (n === 0) return;
      if (e.key === "ArrowLeft") {
        setFocusIdx((i) => Math.max(0, i - 1));
      } else if (e.key === "ArrowRight") {
        setFocusIdx((i) => Math.min(n - 1, i + 1));
      } else if (e.key === " ") {
        e.preventDefault();
        const i = Math.min(focusIdxRef.current, n - 1);
        st.toggleDupChecked(st.activeGroupId!, st.groupMembers[i].fileId);
      } else if (/^[1-9]$/.test(e.key)) {
        const idx = Number(e.key) - 1;
        if (idx < n) st.keepOnly(st.activeGroupId!, st.groupMembers[idx].fileId);
      } else if (e.key === "Enter") {
        const groups = st.dupGroups ?? [];
        const cur = groups.findIndex((g) => g.id === st.activeGroupId);
        if (cur >= 0 && cur + 1 < groups.length) {
          void st.openDupGroup(groups[cur + 1].id);
        }
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  if (groupId == null) {
    return (
      <div className="flex flex-1 items-center justify-center p-6 text-center text-sm text-neutral-600">
        {t("dedup.pickGroup")}
      </div>
    );
  }
  if (members.length === 0) {
    return <div className="flex-1 p-6 text-sm text-neutral-500">…</div>;
  }

  const best = {
    res: Math.max(...members.map((m) => (m.width ?? 0) * (m.height ?? 0))),
    size: Math.max(...members.map((m) => m.size)),
    oldest: Math.min(...members.map((m) => m.takenAt ?? m.mtime)),
  };
  const cols = Math.min(4, Math.max(2, members.length));

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="min-h-0 flex-1 overflow-y-auto p-3">
        <div
          className="grid gap-3"
          style={{ gridTemplateColumns: `repeat(${cols}, minmax(0, 1fr))` }}
        >
          {members.map((m, i) => (
            <MemberCard
              key={m.fileId}
              m={m}
              groupId={groupId}
              zoom={zoom}
              onZoom={setZoom}
              focused={i === focusIdx}
              best={best}
              tz={tz}
              tzOffset={tzOffset}
            />
          ))}
        </div>
      </div>
      <div className="shrink-0 border-t border-neutral-800 px-3 py-1 text-[11px] text-neutral-600">
        {t("dedup.keys")}
      </div>
    </div>
  );
}

export function DedupView() {
  const { t } = useTranslation();
  const stats = useStore((s) => s.dupStats);
  const checked = useStore((s) => s.dupChecked);
  const groups = useStore((s) => s.dupGroups);
  const rule = useStore((s) => s.dedupRule);
  const deleting = useStore((s) => s.dupDeleting);
  const marking = useStore((s) => s.dupMarking);
  const activeJobs = useStore((s) => s.activeJobs);
  const hashJob = [...activeJobs.values()].find((j) => j.kind === "hash");
  const deleteJob = [...activeJobs.values()].find((j) => j.kind === "dedup_delete");
  const anyHashJob = [...activeJobs.values()].some(
    (j) => j.kind === "hash" || j.kind === "org_hash",
  );

  let totalChecked = 0;
  for (const s of checked.values()) totalChecked += s.size;
  // Ước lượng bytes giải phóng: size các bản checked của nhóm đang biết size
  let freed = 0;
  let markedGroups = 0;
  let fullGroups = 0;
  if (groups) {
    const bySize = new Map(groups.map((g) => [g.id, g.size]));
    for (const [gid, s] of checked) {
      freed += (bySize.get(gid) ?? 0) * s.size;
    }
    for (const g of groups) {
      const n = checked.get(g.id)?.size ?? 0;
      if (n > 0) {
        markedGroups++;
        if (n >= g.count - 1) fullGroups++;
      }
    }
  }
  const allState: "none" | "partial" | "all" =
    groups != null && groups.length > 0 && fullGroups === groups.length
      ? "all"
      : markedGroups > 0
        ? "partial"
        : "none";

  // Dùng chung cho nút Delete và phím Del - đọc thẳng store nên không bao giờ
  // xóa theo một snapshot cũ.
  const confirmAndDelete = () => {
    const st = useStore.getState();
    if (st.dupDeleting) return;
    let n = 0;
    for (const s of st.dupChecked.values()) n += s.size;
    if (n === 0) return;
    if (
      [...st.activeJobs.values()].some(
        (j) => j.kind === "hash" || j.kind === "org_hash",
      )
    ) {
      return;
    }
    const bySize = new Map((st.dupGroups ?? []).map((g) => [g.id, g.size]));
    let bytes = 0;
    for (const [gid, s] of st.dupChecked) bytes += (bySize.get(gid) ?? 0) * s.size;
    if (window.confirm(t("dedup.confirm", { n, size: fmtSize(bytes) }))) {
      runSafe(() => useStore.getState().deleteChecked());
    }
  };

  // Phím cấp danh sách (Ctrl+A / Ctrl+D / Esc / Del). Phím cấp bản sao
  // (Space/1-9/mũi tên) nằm trong GroupCompare - tách ra để không đá nhau.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const el = e.target as HTMLElement | null;
      if (
        el &&
        (el.tagName === "INPUT" || el.tagName === "TEXTAREA" || el.tagName === "SELECT")
      ) {
        return;
      }
      if (useStore.getState().appMode !== "dedup") return;
      const ctrl = e.ctrlKey || e.metaKey;
      if (ctrl && (e.key === "a" || e.key === "A")) {
        e.preventDefault();
        runSafe(() => useStore.getState().setAllChecked(true));
      } else if ((ctrl && (e.key === "d" || e.key === "D")) || e.key === "Escape") {
        e.preventDefault();
        runSafe(() => useStore.getState().setAllChecked(false));
      } else if (e.key === "Delete") {
        e.preventDefault();
        confirmAndDelete();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [t]);

  // Rời tab Dedup: xả hàng đợi thumb (ảnh mẫu + card) để job nền không phải
  // giành ổ đĩa với những request không ai còn nhìn.
  useEffect(() => {
    return () => {
      void api.clearThumbQueue();
    };
  }, []);

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      {/* Header: quét + chọn tất cả + rule + xóa */}
      <div className="flex flex-wrap items-center gap-2 border-b border-neutral-800 bg-neutral-950 px-3 py-2">
        <button
          className="rounded border border-neutral-600 bg-neutral-800 px-3 py-1 text-sm text-neutral-100 hover:bg-neutral-700 disabled:opacity-50"
          disabled={anyHashJob}
          onClick={() =>
            runSafe(async () => {
              await api.startHashScan();
            })
          }
        >
          {hashJob != null
            ? `${
                hashJob.message === "verify"
                  ? t("dedup.phaseVerify")
                  : t("dedup.phaseQuick")
              } ${fmtCount(Number(hashJob.done))}${
                hashJob.total != null ? ` / ${fmtCount(Number(hashJob.total))}` : ""
              }`
            : t("dedup.scan")}
        </button>
        {stats && (
          <span className="text-sm text-neutral-400">
            {t("dedup.stats", { n: fmtCount(stats.groups), size: fmtSize(stats.waste) })}
            {hashJob != null && (
              <span className="ml-1 text-xs text-emerald-500">● {t("dedup.live")}</span>
            )}
          </span>
        )}
        <label className="ml-3 flex cursor-pointer items-center gap-1.5 text-sm text-neutral-300">
          <TriCheckbox
            state={allState}
            disabled={marking || anyHashJob || deleting || (groups?.length ?? 0) === 0}
            onChange={(c) => runSafe(() => useStore.getState().setAllChecked(c))}
          />
          {t("dedup.selectAll")}
          {groups != null && groups.length > 0 && (
            <span className="text-xs text-neutral-500">
              {fmtCount(markedGroups)} / {fmtCount(groups.length)}
            </span>
          )}
          {marking && <span className="text-xs text-neutral-500">…</span>}
        </label>
        <div className="ml-auto flex items-center gap-2">
          <select
            className="rounded border border-neutral-700 bg-neutral-900 px-2 py-1 text-sm text-neutral-200 outline-none"
            value={rule}
            onChange={(e) => useStore.getState().setDedupRule(e.target.value as DedupRule)}
            title={t("dedup.rule")}
          >
            <option value="res">{t("dedup.ruleRes")}</option>
            <option value="oldest">{t("dedup.ruleOldest")}</option>
            <option value="newest">{t("dedup.ruleNewest")}</option>
          </select>
          <button
            className="rounded border border-red-800 bg-red-950 px-3 py-1 text-sm text-red-200 hover:bg-red-900 disabled:opacity-40"
            disabled={totalChecked === 0 || deleting || anyHashJob}
            onClick={confirmAndDelete}
          >
            {deleteJob != null
              ? `🗑 ${t("dedup.deleting", {
                  done: fmtCount(Number(deleteJob.done)),
                  total: fmtCount(Number(deleteJob.total ?? 0)),
                })}`
              : `🗑 ${t("dedup.deleteBtn", { n: totalChecked, size: fmtSize(freed) })}`}
          </button>
        </div>
      </div>

      <div className="flex min-h-0 flex-1">
        <aside className="flex w-80 shrink-0 flex-col border-r border-neutral-800 bg-neutral-950">
          <GroupList />
        </aside>
        <GroupCompare />
      </div>
    </div>
  );
}
