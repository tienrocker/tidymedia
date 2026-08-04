/// Mọi chuyển đổi ngày-giờ đi qua timezone user chọn trong Setup (mặc định
/// system tz). KHÔNG dùng `new Date("yyyy-mm-dd")` - nó parse UTC và làm lệch
/// ranh giới ngày theo múi giờ (7 tiếng với UTC+7).

export const systemTzOffsetMinutes = () => -new Date().getTimezoneOffset();

/** "2026-08-03" (theo tz đã chọn) → epoch ms. endOfDay: 23:59:59.999 của ngày đó. */
export function dateInputToEpoch(
  s: string,
  tzOffsetMin: number,
  endOfDay = false,
): number | undefined {
  if (!s) return undefined;
  const [y, m, d] = s.split("-").map(Number);
  if (!y || !m || !d) return undefined;
  const base = Date.UTC(y, m - 1, d) - tzOffsetMin * 60_000;
  return endOfDay ? base + 24 * 3_600_000 - 1 : base;
}

/** epoch ms → "yyyy-mm-dd" theo tz đã chọn (cho input[type=date]). */
export function epochToDateInput(ms: number | undefined, tzOffsetMin: number): string {
  if (ms == null) return "";
  return new Date(ms + tzOffsetMin * 60_000).toISOString().slice(0, 10);
}

/** epoch ms → "yyyy-mm-dd hh:mm" theo tz đã chọn. */
export function fmtDateTime(ms: number, tzOffsetMin: number): string {
  const d = new Date(ms + tzOffsetMin * 60_000);
  if (isNaN(d.getTime())) return "-";
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getUTCFullYear()}-${p(d.getUTCMonth() + 1)}-${p(d.getUTCDate())} ${p(
    d.getUTCHours(),
  )}:${p(d.getUTCMinutes())}`;
}

/** Danh sách offset cho Setup wizard: UTC-12..UTC+14, cả các múi lẻ phổ biến. */
export function tzOptions(): { minutes: number; label: string }[] {
  const list: number[] = [];
  for (let h = -12; h <= 14; h++) list.push(h * 60);
  for (const extra of [330, 345, 390, 525, 570, 630, 825]) list.push(extra);
  list.sort((a, b) => a - b);
  return list.map((minutes) => {
    const sign = minutes < 0 ? "-" : "+";
    const abs = Math.abs(minutes);
    const h = String(Math.floor(abs / 60)).padStart(2, "0");
    const m = String(abs % 60).padStart(2, "0");
    return { minutes, label: `UTC${sign}${h}:${m}` };
  });
}
