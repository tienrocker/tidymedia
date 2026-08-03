import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { open } from "@tauri-apps/plugin-dialog";
import { LANGS } from "../../i18n";
import { api } from "../../lib/ipc";
import { systemTzOffsetMinutes, tzOptions } from "../../lib/time";
import { runSafe, useStore } from "../../state/store";

const LANG_LABELS: Record<string, string> = {
  en: "English",
  vi: "Tiếng Việt",
  zh: "中文",
};

/** Dùng cho cả first-run (bắt buộc, không đóng được) lẫn chỉnh sửa từ nút ⚙. */
export function SettingsDialog({
  firstRun,
  onClose,
}: {
  firstRun: boolean;
  onClose?: () => void;
}) {
  const { t, i18n } = useTranslation();
  const storedTz = useStore((s) => s.tzOffsetMinutes);
  const [lang, setLang] = useState(i18n.resolvedLanguage ?? "en");
  const [tz, setTz] = useState(firstRun ? systemTzOffsetMinutes() : storedTz);
  const [excluded, setExcluded] = useState<string[]>([]);
  const [excludedChanged, setExcludedChanged] = useState(false);
  const systemTz = systemTzOffsetMinutes();

  useEffect(() => {
    api.getExcludedPaths().then(setExcluded).catch(console.error);
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
      await api.setExcludedPaths(excluded);
      await useStore.getState().saveSettings(tz);
      if (excludedChanged && !firstRun) {
        useStore.getState().showToast(t("wizard.rescanHint"), false);
      }
      onClose?.();
    });
  };

  return (
    <div className="fixed inset-0 z-40 flex items-center justify-center bg-black/70">
      <div className="max-h-[85vh] w-[26rem] overflow-y-auto rounded-lg border border-neutral-700 bg-neutral-900 p-6 shadow-2xl">
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
          onChange={(e) => setTz(Number(e.target.value))}
        >
          {tzOptions().map((o) => (
            <option key={o.minutes} value={o.minutes}>
              {o.label}
              {o.minutes === systemTz ? ` — ${t("wizard.systemDefault")}` : ""}
            </option>
          ))}
        </select>

        <div className="mt-4 flex items-center justify-between">
          <label className="block text-xs font-semibold uppercase tracking-wide text-neutral-500">
            {t("wizard.excluded")}
          </label>
          <button
            onClick={addExcluded}
            className="rounded bg-neutral-800 px-2 py-0.5 text-xs text-neutral-200 hover:bg-neutral-700"
          >
            {t("roots.add")}
          </button>
        </div>
        <div className="mt-1 flex max-h-36 flex-col gap-1 overflow-y-auto">
          {excluded.length === 0 && (
            <div className="text-xs text-neutral-600">{t("wizard.excludedEmpty")}</div>
          )}
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
                className="rounded px-1 text-xs text-red-400 hover:bg-neutral-800"
                title={t("roots.remove")}
              >
                ✕
              </button>
            </div>
          ))}
        </div>

        <button
          onClick={onSave}
          className="mt-6 w-full rounded bg-emerald-700 py-2 font-semibold text-white hover:bg-emerald-600"
        >
          {firstRun ? t("wizard.start") : t("wizard.save")}
        </button>
      </div>
    </div>
  );
}
