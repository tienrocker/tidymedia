import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { open } from "@tauri-apps/plugin-dialog";
import { LANGS } from "../../i18n";
import { api } from "../../lib/ipc";
import { fmtCount, fmtSize } from "../../lib/format";
import { systemTimeZone, tzOptions } from "../../lib/time";
import { runSafe, useStore } from "../../state/store";

const LANG_LABELS: Record<string, string> = {
  en: "English",
  vi: "Tiếng Việt",
  zh: "中文",
};

/** Chép kho dữ liệu giữa hai bản cài (bản dev và bản chính thức ghi vào hai
 *  thư mục tách hẳn nhau, nên chuyển bản = quét lại từ đầu nếu không có cái này).
 *  Nhập là THAY THẾ toàn bộ index nên phải soi trước rồi user tự xác nhận. */
function DataTransfer() {
  const { t } = useTranslation();
  const [busy, setBusy] = useState(false);
  const jobsRunning = useStore((s) => s.activeJobs.size > 0);

  const onExport = () =>
    runSafe(async () => {
      const dir = await open({ directory: true, title: t("transfer.exportPick") });
      if (typeof dir !== "string") return;
      setBusy(true);
      try {
        const r = await api.exportData(dir, true);
        useStore
          .getState()
          .showToast(
            t("transfer.exportDone", {
              size: fmtSize(r.indexBytes + r.thumbsBytes),
              dir,
            }),
            false,
          );
      } finally {
        setBusy(false);
      }
    });

  const onImport = () =>
    runSafe(async () => {
      const dir = await open({ directory: true, title: t("transfer.importPick") });
      if (typeof dir !== "string") return;
      setBusy(true);
      try {
        const info = await api.inspectImport(dir);
        if (!info.compatible) {
          useStore.getState().showToast(t(`errors.${info.reason}`), true);
          return;
        }
        const ok = window.confirm(
          t("transfer.importConfirm", {
            files: fmtCount(info.files),
            roots: info.roots.join("\n  ") || "-",
            size: fmtSize(info.indexBytes + info.thumbsBytes),
            thumbs: info.thumbsBytes > 0 ? t("transfer.withThumbs") : t("transfer.noThumbs"),
          }),
        );
        if (!ok) return;
        // Backend khởi động lại app ngay - lời gọi này không bao giờ trả về
        await api.applyImport(dir);
      } finally {
        setBusy(false);
      }
    });

  return (
    <>
      <label className="mt-4 block text-xs font-semibold uppercase tracking-wide text-neutral-500">
        {t("transfer.title")}
      </label>
      <p className="mt-1 text-xs text-neutral-600">{t("transfer.hint")}</p>
      <div className="mt-2 flex gap-2">
        <button
          onClick={onExport}
          disabled={busy}
          className="flex-1 rounded bg-neutral-800 px-2 py-1 text-xs text-neutral-200 hover:bg-neutral-700 disabled:opacity-50"
        >
          {t("transfer.export")}
        </button>
        <button
          onClick={onImport}
          disabled={busy || jobsRunning}
          title={jobsRunning ? t("transfer.importBusy") : undefined}
          className="flex-1 rounded bg-neutral-800 px-2 py-1 text-xs text-neutral-200 hover:bg-neutral-700 disabled:opacity-50"
        >
          {t("transfer.import")}
        </button>
      </div>
    </>
  );
}

/** Dùng cho cả first-run (bắt buộc, không đóng được) lẫn chỉnh sửa từ nút ⚙. */
export function SettingsDialog({
  firstRun,
  onClose,
}: {
  firstRun: boolean;
  onClose?: () => void;
}) {
  const { t, i18n } = useTranslation();
  const storedTz = useStore((s) => s.timezone);
  const [lang, setLang] = useState(i18n.resolvedLanguage ?? "en");
  const [tz, setTz] = useState(firstRun ? systemTimeZone() : storedTz);
  const [excluded, setExcluded] = useState<string[]>([]);
  const [excludedChanged, setExcludedChanged] = useState(false);
  const [excludedLoaded, setExcludedLoaded] = useState(false);
  const [excludedLoadFailed, setExcludedLoadFailed] = useState(false);
  const systemTz = systemTimeZone();

  const loadExcluded = () => {
    setExcludedLoaded(false);
    setExcludedLoadFailed(false);
    api
      .getExcludedPaths()
      .then((paths) => {
        setExcluded(paths);
        setExcludedLoaded(true);
      })
      .catch((e) => {
        setExcludedLoadFailed(true);
        useStore.getState().showToast(String(e), true);
      });
  };

  useEffect(() => {
    loadExcluded();
  }, []);

  // Focus trap tối giản: Tab quay vòng trong dialog, không lọt ra background
  // (background vẫn bấm được bằng bàn phím = kích hoạt được cả nút Delete
  // của dedup từ dưới lớp modal); focus phần tử đầu khi mở.
  const boxRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const box = boxRef.current;
    if (!box) return;
    const focusables = () =>
      [...box.querySelectorAll<HTMLElement>("button, select, input, [tabindex]")].filter(
        (el) => !el.hasAttribute("disabled"),
      );
    focusables()[0]?.focus();
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Tab") return;
      const els = focusables();
      if (els.length === 0) return;
      const first = els[0];
      const last = els[els.length - 1];
      const active = document.activeElement as HTMLElement | null;
      if (!active || !box.contains(active)) {
        e.preventDefault();
        first.focus();
      } else if (!e.shiftKey && active === last) {
        e.preventDefault();
        first.focus();
      } else if (e.shiftKey && active === first) {
        e.preventDefault();
        last.focus();
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, []);

  const addExcluded = () => {
    runSafe(async () => {
      const dir = await open({ directory: true, title: t("wizard.excludePick") });
      if (typeof dir === "string" && !excluded.includes(dir)) {
        setExcluded([...excluded, dir]);
        setExcludedChanged(true);
      }
    });
  };

  const removeExcluded = (p: string) => {
    setExcluded(excluded.filter((x) => x !== p));
    setExcludedChanged(true);
  };

  const onSave = () => {
    runSafe(async () => {
      await i18n.changeLanguage(lang);
      if (excludedChanged) {
        useStore.setState({ orgPreview: null });
        await api.setExcludedPaths(excluded);
      }
      await useStore.getState().saveSettings(tz);
      if (excludedChanged && !firstRun) {
        useStore.getState().showToast(t("wizard.rescanHint"), false);
      }
      onClose?.();
    });
  };

  return (
    <div className="fixed inset-0 z-40 flex items-center justify-center bg-black/70">
      <div
        ref={boxRef}
        className="max-h-[85vh] w-[26rem] overflow-y-auto rounded-lg border border-neutral-700 bg-neutral-900 p-6 shadow-2xl"
      >
        <div className="flex items-start justify-between">
          <h1 className="text-lg font-semibold text-neutral-100">
            {firstRun ? t("wizard.title") : t("wizard.settingsTitle")}
          </h1>
          {!firstRun && (
            <button
              onClick={onClose}
              className="rounded px-1.5 text-neutral-500 hover:bg-neutral-800 hover:text-neutral-200"
              title={t("wizard.cancel")}
            >
              ✕
            </button>
          )}
        </div>
        {firstRun && (
          <p className="mt-1 text-xs text-neutral-500">{t("wizard.subtitle")}</p>
        )}

        <label className="mt-5 block text-xs font-semibold uppercase tracking-wide text-neutral-500">
          {t("wizard.language")}
        </label>
        <select
          className="mt-1 w-full rounded border border-neutral-700 bg-neutral-950 px-2 py-1.5 text-neutral-200 outline-none focus:border-neutral-500"
          value={lang}
          onChange={(e) => {
            setLang(e.target.value);
            void i18n.changeLanguage(e.target.value); // preview ngay
          }}
        >
          {LANGS.map((l) => (
            <option key={l} value={l}>
              {LANG_LABELS[l] ?? l}
            </option>
          ))}
        </select>

        <label className="mt-4 block text-xs font-semibold uppercase tracking-wide text-neutral-500">
          {t("wizard.timezone")}
        </label>
        <select
          className="mt-1 w-full rounded border border-neutral-700 bg-neutral-950 px-2 py-1.5 text-neutral-200 outline-none focus:border-neutral-500"
          value={tz}
          onChange={(e) => setTz(e.target.value)}
        >
          {tzOptions(storedTz).map((o) => (
            <option key={o.id} value={o.id}>
              {o.label}
              {o.id === systemTz ? ` - ${t("wizard.systemDefault")}` : ""}
            </option>
          ))}
        </select>

        <div className="mt-4 flex items-center justify-between">
          <label className="block text-xs font-semibold uppercase tracking-wide text-neutral-500">
            {t("wizard.excluded")}
          </label>
          <button
            onClick={addExcluded}
            disabled={!excludedLoaded}
            className="rounded bg-neutral-800 px-2 py-0.5 text-xs text-neutral-200 hover:bg-neutral-700"
          >
            {t("roots.add")}
          </button>
        </div>
        <div className="mt-1 flex max-h-36 flex-col gap-1 overflow-y-auto">
          {excludedLoadFailed ? (
            <div className="flex items-center justify-between gap-2 text-xs text-amber-400">
              <span>{t("wizard.excludedLoadFailed")}</span>
              <button
                onClick={loadExcluded}
                className="rounded bg-neutral-800 px-2 py-0.5 text-neutral-200 hover:bg-neutral-700"
              >
                {t("wizard.retry")}
              </button>
            </div>
          ) : excluded.length === 0 ? (
            <div className="text-xs text-neutral-600">{t("wizard.excludedEmpty")}</div>
          ) : null}
          {excluded.map((p) => (
            <div
              key={p}
              className="group flex items-center gap-2 rounded border border-neutral-800 bg-neutral-950 px-2 py-1"
            >
              <span className="min-w-0 flex-1 truncate text-xs text-neutral-300" title={p}>
                {p}
              </span>
              <button
                onClick={() => removeExcluded(p)}
                disabled={!excludedLoaded}
                className="rounded px-1 text-xs text-red-400 hover:bg-neutral-800"
                title={t("roots.remove")}
              >
                ✕
              </button>
            </div>
          ))}
        </div>

        {/* First-run chưa có gì để xuất, mà nhập lúc đó là ghi đè kho vừa tạo */}
        {!firstRun && <DataTransfer />}

        <button
          onClick={onSave}
          disabled={!excludedLoaded && !(excludedLoadFailed && !excludedChanged)}
          className="mt-6 w-full rounded bg-emerald-700 py-2 font-semibold text-white hover:bg-emerald-600 disabled:opacity-50"
        >
          {firstRun ? t("wizard.start") : t("wizard.save")}
        </button>
      </div>
    </div>
  );
}
