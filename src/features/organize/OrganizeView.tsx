import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { open } from "@tauri-apps/plugin-dialog";
import { runSafe, useStore } from "../../state/store";
import { api } from "../../lib/ipc";
import { errText } from "../../lib/errors";
import { fmtCount } from "../../lib/format";
import { fmtDateTime } from "../../lib/time";

const inputCls =
  "rounded border border-neutral-700 bg-neutral-900 px-2 py-1 text-sm text-neutral-200 " +
  "placeholder-neutral-500 outline-none focus:border-neutral-500";

function Section({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <section className="rounded-lg border border-neutral-800 bg-neutral-950 p-4">
      <h2 className="mb-3 text-xs font-semibold uppercase tracking-wide text-neutral-500">
        {title}
      </h2>
      {children}
    </section>
  );
}

/** Thư mục đích: user tự chọn, mỗi ổ 1 thư mục, tên gì cũng được. */
function LibraryRoots() {
  const { t } = useTranslation();
  const roots = useStore((s) => s.libRoots);

  const pick = () => {
    runSafe(async () => {
      const dir = await open({ directory: true, title: t("org.pickRoot") });
      if (typeof dir === "string") {
        await useStore.getState().addLibraryRoot(dir);
      }
    });
  };

  return (
    <Section title={t("org.roots")}>
      <p className="mb-2 text-xs text-neutral-500">{t("org.rootsHint")}</p>
      <div className="flex flex-col gap-1">
        {roots.length === 0 && (
          <div className="text-sm text-amber-400">{t("org.noRoots")}</div>
        )}
        {roots.map((r) => (
          <div
            key={r.id}
            className="flex items-center gap-2 rounded border border-neutral-800 bg-neutral-900 px-2 py-1.5"
          >
            <span className="min-w-0 flex-1 truncate text-sm text-neutral-200" title={r.path}>
              {r.path}
            </span>
            {r.isPrimary && (
              <span className="rounded bg-emerald-900/60 px-1.5 text-[10px] font-semibold text-emerald-300">
                {t("org.primary")}
              </span>
            )}
            <button
              className="rounded px-1 text-xs text-red-400 hover:bg-neutral-800"
              title={t("roots.remove")}
              onClick={() =>
                runSafe(() => useStore.getState().removeLibraryRoot(r.id))
              }
            >
              ✕
            </button>
          </div>
        ))}
      </div>
      <button
        onClick={pick}
        className="mt-2 rounded bg-neutral-800 px-3 py-1 text-sm text-neutral-200 hover:bg-neutral-700"
      >
        + {t("org.addRoot")}
      </button>
    </Section>
  );
}

/** Thư mục NGUỒN được phép gom. Rỗng = cả ổ.
 *
 * Không có bộ này thì bấm Gom là ôm mọi file trên ổ — kể cả thư mục video tải
 * về hay ảnh cưới của studio, những thứ không thuộc cây theo ngày. Undo được,
 * nhưng undo hàng chục nghìn file vẫn là một mớ.
 */
function SourceScopes() {
  const { t } = useTranslation();
  const scopes = useStore((s) => s.orgSettings?.scopes) ?? [];

  const add = () =>
    runSafe(async () => {
      const dir = await open({ directory: true, title: t("org.pickScope") });
      if (typeof dir === "string") {
        await useStore.getState().setOrgScopes([...scopes, dir]);
      }
    });

  return (
    <Section title={t("org.scopes")}>
      <p className="mb-2 text-xs text-neutral-500">{t("org.scopesHint")}</p>
      {scopes.length === 0 ? (
        <div className="rounded border border-amber-900 bg-amber-950/40 px-2 py-1.5 text-xs text-amber-300">
          {t("org.scopesAll")}
        </div>
      ) : (
        <div className="flex flex-col gap-1">
          {scopes.map((p) => (
            <div
              key={p}
              className="flex items-center gap-2 rounded border border-neutral-800 bg-neutral-900 px-2 py-1.5"
            >
              <span className="min-w-0 flex-1 truncate text-sm text-neutral-200" title={p}>
                {p}
              </span>
              <button
                className="rounded px-1 text-xs text-red-400 hover:bg-neutral-800"
                title={t("roots.remove")}
                onClick={() =>
                  runSafe(() =>
                    useStore.getState().setOrgScopes(scopes.filter((x) => x !== p)),
                  )
                }
              >
                ✕
              </button>
            </div>
          ))}
        </div>
      )}
      <button
        onClick={add}
        className="mt-2 rounded bg-neutral-800 px-3 py-1 text-sm text-neutral-200 hover:bg-neutral-700"
      >
        + {t("org.addScope")}
      </button>
    </Section>
  );
}

/** Preset 1-click — giá trị PHẢI khớp DEFAULT_*_TEMPLATE (template.rs) và token mới. */
const TEMPLATE_PRESETS = [
  { key: "org.presetByDate", dir: "{YYYY}\\{YYYY}-{MM}", file: "{YYYYMMDD}_{hhmmss}_{hash4}" },
  { key: "org.presetKeepStructure", dir: "{relpath}", file: "{YYYYMMDD}_{hhmmss}_{hash4}" },
  { key: "org.presetKeepStructureName", dir: "{relpath}", file: "{name}" },
  { key: "org.presetByDevice", dir: "{camera}\\{YYYY}-{MM}", file: "{YYYYMMDD}_{hhmmss}_{hash4}" },
  // Địa điểm: {place} là thành phố, đủ dùng cho ảnh đi xa. Muốn chi tiết tới
  // phường thì gõ tay {province}\{district}\{ward} — chỉ VN có tầng đó, để
  // preset lại thì user nước khác bấm vào ra một tầng thư mục rỗng.
  { key: "org.presetByPlace", dir: "{YYYY}\\{place}", file: "{YYYYMMDD}_{hhmmss}_{hash4}" },
  // Ảnh/Video → năm → tháng. KHÔNG có token hash: hash bắt phải đọc vân tay nội
  // dung mọi file (hàng giờ trên ổ cơ) mà thứ nó mua được là SkipDuplicate —
  // vô dụng với kho gần như không có bản trùng byte. Ảnh cùng giây sẽ nhận hậu
  // tố _2, không mất file nào, và tab Gần giống dọn sau.
  { key: "org.presetByKind", dir: "{kind}\\{YYYY}\\{YYYY}-{MM}", file: "{YYYYMMDD}_{hhmmss}" },
] as const;

/** Template thư mục + tên file, validate + render ví dụ REALTIME khi gõ. */
function TemplateSettings() {
  const { t } = useTranslation();
  const settings = useStore((s) => s.orgSettings);
  const [dir, setDir] = useState<string | null>(null);
  const [file, setFile] = useState<string | null>(null);
  const [live, setLive] = useState<
    { sample: string } | { error: string } | null
  >(null);

  // Local edit state init từ settings load xong
  const dirVal = dir ?? settings?.dirTemplate ?? "";
  const fileVal = file ?? settings?.fileTemplate ?? "";
  const dirty =
    settings != null &&
    (dirVal !== settings.dirTemplate || fileVal !== settings.fileTemplate);
  const hasSettings = settings != null;

  // Ví dụ chạy theo INPUT (debounce 250ms), không đợi Lưu — lỗi template
  // cũng hiện ngay tại chỗ thay vì toast lúc bấm Lưu
  useEffect(() => {
    if (!hasSettings) return;
    const timer = window.setTimeout(() => {
      api
        .previewOrgTemplate(dirVal, fileVal)
        .then((sample) => setLive({ sample }))
        .catch((e) => setLive({ error: errText(e) }));
    }, 250);
    return () => window.clearTimeout(timer);
  }, [dirVal, fileVal, hasSettings]);

  const liveError = live != null && "error" in live ? live.error : null;
  const liveSample =
    live != null && "sample" in live ? live.sample : settings?.sample;
  // Không có token hash trong tên thì planner không bao giờ được phép kết luận
  // "cùng nội dung" (2 file chưa biết ruột mà trùng tên phải tách ra, không đè).
  // Hệ quả: gom lần 2 từ cùng nguồn sẽ ra bản _2 thay vì báo trùng.
  const noHashToken = !/\{hash(4|8|16)\}/.test(fileVal);

  return (
    <Section title={t("org.template")}>
      <p className="mb-2 text-xs text-neutral-500">{t("org.templateHint")}</p>
      <div className="flex flex-col gap-2">
        <div className="flex flex-wrap items-center gap-1.5 text-xs">
          <span className="text-neutral-500">{t("org.presets")}:</span>
          {TEMPLATE_PRESETS.map((p) => {
            const active = dirVal === p.dir && fileVal === p.file;
            return (
              <button
                key={p.key}
                className={`rounded border px-2 py-0.5 ${
                  active
                    ? "border-emerald-700 bg-emerald-900/40 text-emerald-300"
                    : "border-neutral-700 bg-neutral-900 text-neutral-300 hover:bg-neutral-800"
                }`}
                onClick={() => {
                  setDir(p.dir);
                  setFile(p.file);
                }}
              >
                {t(p.key)}
              </button>
            );
          })}
        </div>
        <label className="flex items-center gap-2 text-sm text-neutral-400">
          <span className="w-28 shrink-0">{t("org.dirTemplate")}</span>
          <input
            className={`${inputCls} flex-1 font-mono`}
            value={dirVal}
            onChange={(e) => setDir(e.target.value)}
            spellCheck={false}
          />
        </label>
        <label className="flex items-center gap-2 text-sm text-neutral-400">
          <span className="w-28 shrink-0">{t("org.fileTemplate")}</span>
          <input
            className={`${inputCls} flex-1 font-mono`}
            value={fileVal}
            onChange={(e) => setFile(e.target.value)}
            spellCheck={false}
          />
        </label>
        <div className="text-xs text-neutral-500">
          {t("org.tokens")}:{" "}
          <code className="text-neutral-400">
            {"{YYYY} {MM} {DD} {YYYYMMDD} {hh} {mm} {ss} {hhmmss} {hash4} {hash8} {hash16} {camera} {kind} {relpath} {folder} {name} {place} {province} {district} {ward} {country}"}
          </code>
        </div>
        <div className="text-xs text-neutral-500">{t("org.tokensHint")}</div>
        {noHashToken && (
          <div className="rounded border border-amber-900 bg-amber-950/40 px-2 py-1.5 text-xs text-amber-300">
            {t("org.noHashTokenHint")}
          </div>
        )}
        {liveError != null ? (
          <div className="rounded border border-red-900 bg-red-950/60 px-2 py-1.5 text-sm text-red-300">
            {liveError}
          </div>
        ) : (
          liveSample != null && (
            <div className="rounded bg-neutral-900 px-2 py-1.5 text-sm">
              <span className="text-neutral-500">{t("org.sample")}: </span>
              <span className="font-mono text-emerald-300">{liveSample}</span>
            </div>
          )
        )}
        {dirty && (
          <button
            className="self-start rounded bg-emerald-700 px-3 py-1 text-sm font-semibold text-white hover:bg-emerald-600 disabled:opacity-50"
            disabled={liveError != null}
            onClick={() =>
              runSafe(async () => {
                await useStore.getState().saveOrgSettings(dirVal, fileVal);
                setDir(null);
                setFile(null);
              })
            }
          >
            {t("wizard.save")}
          </button>
        )}
      </div>
    </Section>
  );
}

function Chip({ label, n, tone }: { label: string; n: number; tone?: string }) {
  if (n === 0) return null;
  return (
    <span
      className={`rounded px-2 py-0.5 text-xs ${tone ?? "bg-neutral-800 text-neutral-300"}`}
    >
      {label}: {fmtCount(n)}
    </span>
  );
}

/** Dry-run bắt buộc + bảng old -> new + nút chạy thật. */
function PreviewAndRun() {
  const { t } = useTranslation();
  const preview = useStore((s) => s.orgPreview);
  const busy = useStore((s) => s.orgBusy);
  const previewing = useStore((s) => s.orgPreviewing);
  const includeUncertain = useStore((s) => s.orgIncludeUncertain);
  const libRoots = useStore((s) => s.libRoots);
  const activeJobs = useStore((s) => s.activeJobs);
  const running = [...activeJobs.values()].some(
    (j) =>
      j.kind === "organize" ||
      j.kind === "org_undo" ||
      j.kind === "org_hash" ||
      j.kind === "hash" ||
      j.kind === "scan" ||
      j.kind === "meta" ||
      j.kind === "recovery",
  );

  const actionable = (preview?.moves ?? 0) + (preview?.copies ?? 0);

  return (
    <Section title={t("org.run")}>
      <div className="flex flex-wrap items-center gap-3">
        <label className="flex items-center gap-1.5 text-sm text-neutral-300">
          <input
            type="checkbox"
            checked={includeUncertain}
            onChange={(e) =>
              useStore.getState().setOrgIncludeUncertain(e.target.checked)
            }
          />
          {t("org.includeUncertain")}
        </label>
        <button
          className="rounded bg-neutral-800 px-3 py-1 text-sm text-neutral-200 hover:bg-neutral-700 disabled:opacity-50"
          disabled={busy || running || libRoots.length === 0}
          onClick={() => runSafe(() => useStore.getState().runOrgPreview())}
        >
          {busy ? t("org.previewing") : t("org.preview")}
        </button>
        {previewing && (
          <button
            className="rounded bg-red-950 px-3 py-1 text-sm text-red-300 hover:bg-red-900"
            onClick={() => runSafe(() => useStore.getState().cancelOrgPreview())}
          >
            {t("org.cancelPreview")}
          </button>
        )}
        <button
          className="rounded bg-emerald-700 px-3 py-1 text-sm font-semibold text-white hover:bg-emerald-600 disabled:opacity-50"
          disabled={busy || running || preview == null || actionable === 0}
          title={preview == null ? t("org.previewFirst") : undefined}
          onClick={() => {
            if (
              window.confirm(
                t("org.confirm", { n: fmtCount(actionable) }),
              )
            ) {
              runSafe(() => useStore.getState().startOrganize());
            }
          }}
        >
          {t("org.execute", { n: preview ? fmtCount(actionable) : "?" })}
        </button>
        {preview != null && preview.needsHash > 0 && (
          <button
            className="rounded bg-amber-800 px-3 py-1 text-sm font-semibold text-white hover:bg-amber-700 disabled:opacity-50"
            disabled={busy || running}
            onClick={() => {
              if (
                window.confirm(t("org.hashConfirm"))
              ) {
                runSafe(() => useStore.getState().startOrgHashScan());
              }
            }}
          >
            {t("org.prepareHashes")}
          </button>
        )}
        {running && (
          <span className="text-sm text-amber-400">{t("org.running")}</span>
        )}
      </div>

      {preview && (
        <>
          <div className="mt-3 flex flex-wrap gap-1.5">
            <Chip
              label={t("org.cMoves")}
              n={preview.moves}
              tone="bg-emerald-900/60 text-emerald-300"
            />
            <Chip
              label={t("org.cCopies")}
              n={preview.copies}
              tone="bg-sky-900/60 text-sky-300"
            />
            <Chip
              label={t("org.cNeedsHash")}
              n={preview.needsHash}
              tone="bg-amber-900/60 text-amber-300"
            />
            <Chip label={t("org.cOrganized")} n={preview.skipOrganized} />
            <Chip label={t("org.cDuplicate")} n={preview.skipDuplicate} />
            <Chip label={t("org.cCloud")} n={preview.skipCloud} />
            <Chip label={t("org.cUncertain")} n={preview.skipUncertain} />
            <Chip label={t("org.cPairBlocked")} n={preview.skipPairBlocked} />
            <Chip
              label={t("org.cPathTooLong")}
              n={preview.skipPathTooLong}
              tone="bg-red-900/50 text-red-300"
            />
            <Chip label={t("org.cOther")} n={preview.skipOther} />
          </div>
          {preview.volumesMissingRoot.length > 0 && (
            <div className="mt-2 text-sm text-amber-400">
              {t("org.missingRoots", {
                drives: preview.volumesMissingRoot.join(", "),
              })}
            </div>
          )}
          {preview.sample.length > 0 && (
            <div className="mt-3 max-h-72 overflow-y-auto rounded border border-neutral-800">
              {preview.sample.map((row) => (
                <div
                  key={row.fileId}
                  className="border-b border-neutral-900 px-2 py-1 font-mono text-xs"
                >
                  <div className="truncate text-neutral-500" title={row.oldPath}>
                    {row.oldPath}
                  </div>
                  <div
                    className={`truncate ${
                      row.action === "NEEDS_HASH"
                        ? "text-amber-300"
                        : "text-emerald-300"
                    }`}
                    title={row.newPath ?? ""}
                  >
                    {row.action === "NEEDS_HASH"
                      ? t("org.hashRequired")
                      : `→ ${row.newPath ?? ""}`}
                  </div>
                </div>
              ))}
              {preview.sample.length <
                preview.moves + preview.copies + preview.needsHash && (
                <div className="px-2 py-1 text-xs text-neutral-500">
                  {t("org.sampleCapped", { n: fmtCount(preview.sample.length) })}
                </div>
              )}
            </div>
          )}
        </>
      )}
    </Section>
  );
}

/** Các đợt organize đã chạy + undo từng đợt. */
function Batches() {
  const { t } = useTranslation();
  const batches = useStore((s) => s.orgBatches);
  const tz = useStore((s) => s.timezone);
  const tzOffset = useStore((s) => s.tzOffsetMinutes);
  const activeJobs = useStore((s) => s.activeJobs);
  const running = [...activeJobs.values()].some(
    (j) =>
      j.kind === "organize" ||
      j.kind === "org_undo" ||
      j.kind === "hash" ||
      j.kind === "org_hash" ||
      j.kind === "scan" ||
      j.kind === "recovery",
  );

  const visible = batches.filter((b) => b.moved > 0);
  if (visible.length === 0) return null;
  return (
    <Section title={t("org.history")}>
      <div className="flex flex-col gap-1">
        {visible.map((b) => (
          <div
            key={b.batchId}
            className="flex items-center gap-2 rounded border border-neutral-800 bg-neutral-900 px-2 py-1.5 text-sm"
          >
            <span className="flex-1 text-neutral-300">
              {t("org.batch", { n: fmtCount(b.moved) })}
              {b.undone > 0 && (
                <span className="ml-1 text-neutral-500">
                  ({t("org.undone", { n: fmtCount(b.undone) })})
                </span>
              )}
            </span>
            <span className="text-xs text-neutral-500">
              {b.finishedAt != null ? fmtDateTime(b.finishedAt, tz, tzOffset) : ""}
            </span>
            <button
              className="rounded bg-neutral-800 px-2 py-0.5 text-xs text-neutral-200 hover:bg-neutral-700 disabled:opacity-50"
              disabled={running}
              onClick={() => {
                if (window.confirm(t("org.undoConfirm", { n: fmtCount(b.moved) }))) {
                  runSafe(() => useStore.getState().undoOrgBatch(b.batchId));
                }
              }}
            >
              ↩ {t("org.undo")}
            </button>
          </div>
        ))}
      </div>
    </Section>
  );
}

export function OrganizeView() {
  const { t } = useTranslation();
  const settings = useStore((s) => s.orgSettings);

  useEffect(() => {
    if (useStore.getState().orgSettings == null) {
      void useStore.getState().loadOrgData();
    }
  }, []);

  if (settings == null) {
    return <div className="flex-1 p-6 text-sm text-neutral-500">…</div>;
  }
  return (
    <div className="min-h-0 flex-1 overflow-y-auto">
      <div className="mx-auto flex max-w-3xl flex-col gap-4 p-4">
        <div>
          <h1 className="text-lg font-semibold text-neutral-100">
            {t("org.title")}
          </h1>
          <p className="text-sm text-neutral-500">{t("org.subtitle")}</p>
        </div>
        <LibraryRoots />
        <SourceScopes />
        <TemplateSettings />
        <PreviewAndRun />
        <Batches />
      </div>
    </div>
  );
}
