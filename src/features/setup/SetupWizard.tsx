import { useState } from "react";
import { useTranslation } from "react-i18next";
import { LANGS } from "../../i18n";
import { systemTzOffsetMinutes, tzOptions } from "../../lib/time";
import { runSafe, useStore } from "../../state/store";

const LANG_LABELS: Record<string, string> = {
  en: "English",
  vi: "Tiếng Việt",
  zh: "中文",
};

export function SetupWizard() {
  const { t, i18n } = useTranslation();
  const [lang, setLang] = useState(i18n.resolvedLanguage ?? "en");
  const [tz, setTz] = useState(systemTzOffsetMinutes());
  const systemTz = systemTzOffsetMinutes();

  const onSave = () => {
    runSafe(async () => {
      await i18n.changeLanguage(lang);
      await useStore.getState().saveSettings(tz);
    });
  };

  return (
    <div className="fixed inset-0 z-40 flex items-center justify-center bg-black/70">
      <div className="w-96 rounded-lg border border-neutral-700 bg-neutral-900 p-6 shadow-2xl">
        <h1 className="text-lg font-semibold text-neutral-100">{t("wizard.title")}</h1>
        <p className="mt-1 text-xs text-neutral-500">{t("wizard.subtitle")}</p>

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

        <button
          onClick={onSave}
          className="mt-6 w-full rounded bg-emerald-700 py-2 font-semibold text-white hover:bg-emerald-600"
        >
          {t("wizard.start")}
        </button>
      </div>
    </div>
  );
}
