import i18n from "../i18n";

/** Backend trả lỗi dạng "ERR_CODE|detail" — dịch code, kèm detail. */
export function errText(e: unknown): string {
  const s = e instanceof Error ? e.message : String(e);
  const m = s.match(/(ERR_[A-Z_]+)\|?([\s\S]*)/);
  if (m) {
    const key = `errors.${m[1]}`;
    if (i18n.exists(key)) {
      return i18n.t(key, { detail: m[2].trim() });
    }
  }
  return i18n.t("errors.generic", { detail: s });
}
