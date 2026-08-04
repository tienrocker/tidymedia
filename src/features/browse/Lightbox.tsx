import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { runSafe, useStore } from "../../state/store";
import { api, FileDetail, FileRow } from "../../lib/ipc";
import { canNativeDisplay, mediaUrl, thumbUrl, THUMB_PREVIEW } from "../../lib/media";
import { fmtDuration, fmtSize } from "../../lib/format";
import { fmtDateTime } from "../../lib/time";

interface Zoom {
  scale: number;
  tx: number;
  ty: number;
}
const ZOOM_RESET: Zoom = { scale: 1, tx: 0, ty: 0 };

function srcFor(row: FileRow): string | null {
  if (row.kind !== 0 || row.status !== 0) return null;
  return canNativeDisplay(row.ext)
    ? mediaUrl(row.id, row.mtime)
    : thumbUrl(row.id, THUMB_PREVIEW, row.mtime);
}

function InfoRow({ label, value }: { label: string; value: string | null }) {
  if (!value) return null;
  return (
    <div className="border-b border-neutral-800 py-1.5">
      <div className="text-[10px] uppercase tracking-wide text-neutral-500">{label}</div>
      <div className="break-all text-xs text-neutral-200">{value}</div>
    </div>
  );
}

export function Lightbox() {
  const index = useStore((s) => s.lightboxIndex);
  if (index == null) return null;
  return <LightboxInner index={index} />;
}

function LightboxInner({ index }: { index: number }) {
  const { t } = useTranslation();
  const total = useStore((s) => s.total);
  const rows = useStore((s) => s.rows);
  const tz = useStore((s) => s.timezone);
  const tzOffset = useStore((s) => s.tzOffsetMinutes);
  const row = rows.get(index);

  const [zoom, setZoom] = useState<Zoom>(ZOOM_RESET);
  const [showInfo, setShowInfo] = useState(false);
  const [detail, setDetail] = useState<FileDetail | null>(null);
  const [imgFailed, setImgFailed] = useState(false);
  // media:// fail (ext nói dối nội dung, vd .jpg ruột HEIC) → thử thumb 1600
  const [useThumbFallback, setUseThumbFallback] = useState(false);
  // Video codec WebView2 không phát được (HEVC thiếu extension...) → nút mở app hệ thống
  const [videoFailed, setVideoFailed] = useState(false);
  const drag = useRef<{ x: number; y: number } | null>(null);
  const stageRef = useRef<HTMLDivElement>(null);

  const rowId = row?.id;

  // Đổi ảnh (index hoặc id tại index đổi) → reset zoom/lỗi/fallback
  useEffect(() => {
    setZoom(ZOOM_RESET);
    setImgFailed(false);
    setUseThumbFallback(false);
    setVideoFailed(false);
  }, [index, rowId]);

  // Row thiếu (mở xa viewport, hoặc re-query lúc scan xóa sạch rows cache) →
  // fetch lại. Deps có `row` để spinner tự hồi sau mỗi lần runQuery.
  useEffect(() => {
    if (!row) useStore.getState().ensureRange(Math.max(0, index - 3), index + 3);
  }, [index, row]);

  // Panel info: fetch meta (backend tự trích on-demand nếu job chưa tới file này)
  useEffect(() => {
    setDetail(null);
    if (rowId == null) return;
    let stale = false;
    api
      .getFileMeta(rowId)
      .then((d) => {
        if (!stale) setDetail(d);
      })
      .catch(() => {});
    return () => {
      stale = true;
    };
  }, [rowId]);

  // Preload ảnh kề — bấm next không chờ
  useEffect(() => {
    for (const d of [1, -1]) {
      const r = rows.get(index + d);
      if (r) {
        const src = srcFor(r);
        if (src) new Image().src = src;
      }
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [index, row?.id]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      // Focus còn trong ô search/filter, hoặc trong <video> controls (mũi tên
      // là phím tua của player) → đừng cướp phím
      const el = e.target as HTMLElement | null;
      if (
        el &&
        (el.tagName === "INPUT" ||
          el.tagName === "TEXTAREA" ||
          el.tagName === "SELECT" ||
          el.tagName === "VIDEO")
      ) {
        if (e.key === "Escape") useStore.getState().closeLightbox();
        return;
      }
      if (e.key === "Escape") useStore.getState().closeLightbox();
      else if (e.key === "ArrowLeft") useStore.getState().stepLightbox(-1);
      else if (e.key === "ArrowRight") useStore.getState().stepLightbox(1);
      else if (e.key === "i" || e.key === "I") setShowInfo((v) => !v);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const zoomAt = (clientX: number, clientY: number, factor: number) => {
    if (row?.kind === 1) return; // video: player tự lo, không zoom transform
    const rect = stageRef.current?.getBoundingClientRect();
    if (!rect) return;
    setZoom((z) => {
      const scale = Math.min(10, Math.max(1, z.scale * factor));
      if (scale === 1) return ZOOM_RESET;
      // Zoom quanh con trỏ: giữ điểm dưới trỏ đứng yên
      const px = clientX - rect.left - rect.width / 2;
      const py = clientY - rect.top - rect.height / 2;
      const k = scale / z.scale;
      return { scale, tx: px - (px - z.tx) * k, ty: py - (py - z.ty) * k };
    });
  };

  const primary = row ? srcFor(row) : null;
  const src =
    useThumbFallback && row ? thumbUrl(row.id, THUMB_PREVIEW, row.mtime) : primary;
  const showImg = src != null && !imgFailed;

  return (
    <div className="fixed inset-0 z-40 flex flex-col bg-black/95">
      {/* Toolbar */}
      <div className="flex shrink-0 items-center gap-2 px-3 py-2">
        <span className="truncate text-sm text-neutral-200" title={row?.name}>
          {row?.name ?? "…"}
        </span>
        <span className="truncate text-xs text-neutral-500">{row?.dir}</span>
        <span className="ml-auto shrink-0 text-xs tabular-nums text-neutral-500">
          {index + 1} / {total}
        </span>
        <button
          className={`rounded border px-2 py-0.5 text-xs ${
            showInfo
              ? "border-neutral-500 text-neutral-200"
              : "border-neutral-700 text-neutral-400 hover:text-neutral-200"
          }`}
          onClick={() => setShowInfo((v) => !v)}
          title="i"
        >
          ⓘ {t("lightbox.info")}
        </button>
        <button
          className="rounded border border-neutral-700 px-2 py-0.5 text-xs text-neutral-400 hover:text-neutral-200"
          onClick={() => row && runSafe(() => api.revealFile(row.id))}
          title={t("lightbox.reveal")}
        >
          📂
        </button>
        <button
          className="rounded border border-neutral-700 px-2 py-0.5 text-xs text-neutral-400 hover:text-neutral-200"
          onClick={() => useStore.getState().closeLightbox()}
          title="Esc"
        >
          ✕
        </button>
      </div>

      <div className="flex min-h-0 flex-1">
        {/* Stage */}
        <div
          ref={stageRef}
          className="relative flex min-w-0 flex-1 items-center justify-center overflow-hidden"
          onWheel={(e) => zoomAt(e.clientX, e.clientY, e.deltaY < 0 ? 1.25 : 0.8)}
          onDoubleClick={(e) =>
            zoom.scale === 1 ? zoomAt(e.clientX, e.clientY, 2.5) : setZoom(ZOOM_RESET)
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
              setZoom((z) => ({ ...z, tx: z.tx + dx, ty: z.ty + dy }));
            }
          }}
          onPointerUp={() => {
            drag.current = null;
          }}
        >
          {!row ? (
            <div className="h-8 w-8 animate-spin rounded-full border-2 border-neutral-700 border-t-neutral-300" />
          ) : row.kind === 1 && row.status === 0 && !videoFailed ? (
            <video
              key={row.id}
              src={mediaUrl(row.id, row.mtime)}
              poster={thumbUrl(row.id, THUMB_PREVIEW, row.mtime)}
              controls
              autoPlay
              className="max-h-full max-w-full"
              onError={() => setVideoFailed(true)}
            />
          ) : showImg ? (
            <img
              key={`${row.id}-${useThumbFallback ? "t" : "m"}`}
              src={src}
              alt={row.name}
              draggable={false}
              onError={() => {
                if (!useThumbFallback && canNativeDisplay(row.ext)) {
                  setUseThumbFallback(true);
                } else {
                  setImgFailed(true);
                }
              }}
              className="max-h-full max-w-full select-none object-contain"
              style={{
                transform: `translate(${zoom.tx}px, ${zoom.ty}px) scale(${zoom.scale})`,
                cursor: zoom.scale > 1 ? "grab" : "default",
                transition: drag.current ? "none" : "transform 80ms",
              }}
            />
          ) : (
            <div className="flex flex-col items-center gap-3 text-neutral-500">
              <span className="text-6xl">
                {row.status === 2 ? "☁️" : row.kind === 1 ? "🎬" : "🖼️"}
              </span>
              <span className="text-sm">
                {row.status === 2
                  ? t("lightbox.cloudOnly")
                  : row.kind === 1
                    ? t("lightbox.codecUnsupported")
                    : t("lightbox.noPreview")}
              </span>
              {row.kind === 1 && row.status === 0 && (
                <button
                  className="rounded border border-neutral-600 px-3 py-1.5 text-sm text-neutral-200 hover:bg-neutral-800"
                  onClick={() => runSafe(() => api.openFile(row.id))}
                >
                  ▶ {t("lightbox.openSystem")}
                </button>
              )}
            </div>
          )}

          {/* Prev / Next */}
          {index > 0 && (
            <button
              className="absolute left-2 top-1/2 -translate-y-1/2 rounded-full bg-neutral-900/80 px-3 py-2 text-xl text-neutral-300 hover:bg-neutral-800"
              onClick={() => useStore.getState().stepLightbox(-1)}
            >
              ‹
            </button>
          )}
          {index < total - 1 && (
            <button
              className="absolute right-2 top-1/2 -translate-y-1/2 rounded-full bg-neutral-900/80 px-3 py-2 text-xl text-neutral-300 hover:bg-neutral-800"
              onClick={() => useStore.getState().stepLightbox(1)}
            >
              ›
            </button>
          )}
        </div>

        {/* Info panel */}
        {showInfo && row && (
          <div className="w-72 shrink-0 overflow-y-auto border-l border-neutral-800 bg-neutral-950 px-3 py-2">
            <InfoRow label={t("list.name")} value={row.name} />
            <InfoRow label={t("list.folder")} value={row.dir} />
            <InfoRow
              label={t("lightbox.dimensions")}
              value={
                detail?.width != null && detail?.height != null
                  ? `${detail.width} × ${detail.height}`
                  : row.width != null && row.height != null
                    ? `${row.width} × ${row.height}`
                    : null
              }
            />
            <InfoRow
              label={t("lightbox.duration")}
              value={detail?.durationMs != null ? fmtDuration(detail.durationMs) : null}
            />
            <InfoRow
              label={t("lightbox.codec")}
              value={
                detail?.vcodec
                  ? `${detail.vcodec}${detail.acodec ? ` / ${detail.acodec}` : ""}${
                      detail.fps ? ` · ${Math.round(detail.fps)} fps` : ""
                    }`
                  : null
              }
            />
            <InfoRow label={t("list.size")} value={fmtSize(row.size)} />
            <InfoRow
              label={t("lightbox.taken")}
              value={
                // taken_at là wall-clock camera (khung UTC) → hiển thị với tz=0
                detail?.takenAt != null ? fmtDateTime(detail.takenAt, 0) : null
              }
            />
            <InfoRow label={t("lightbox.camera")} value={detail?.camera ?? null} />
            <InfoRow
              label={t("list.modified")}
              value={fmtDateTime(row.mtime, tz, tzOffset)}
            />
          </div>
        )}
      </div>
    </div>
  );
}
