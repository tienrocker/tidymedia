/** Date/time helpers. IANA timezone IDs are DST-aware; numeric offsets remain
 * supported only for old settings and EXIF wall-clock values (`0`). */

export type TimeZoneSpec = string | number;

export const systemTzOffsetMinutes = () => -new Date().getTimezoneOffset();

export const systemTimeZone = (): string => {
  try {
    return Intl.DateTimeFormat().resolvedOptions().timeZone || "Etc/UTC";
  } catch {
    return "Etc/UTC";
  }
};

const formatters = new Map<string, Intl.DateTimeFormat>();

function formatterFor(timezone: string): Intl.DateTimeFormat {
  let formatter = formatters.get(timezone);
  if (!formatter) {
    formatter = new Intl.DateTimeFormat("en-CA", {
      timeZone: timezone,
      year: "numeric",
      month: "2-digit",
      day: "2-digit",
      hour: "2-digit",
      minute: "2-digit",
      second: "2-digit",
      hourCycle: "h23",
    });
    formatters.set(timezone, formatter);
  }
  return formatter;
}

interface CivilParts {
  y: number;
  m: number;
  d: number;
  hh: number;
  mm: number;
  ss: number;
}

function partsAt(ms: number, timezone: string): CivilParts {
  const parts = formatterFor(timezone).formatToParts(new Date(ms));
  const get = (type: Intl.DateTimeFormatPartTypes) =>
    Number(parts.find((p) => p.type === type)?.value ?? 0);
  return {
    y: get("year"),
    m: get("month"),
    d: get("day"),
    hh: get("hour"),
    mm: get("minute"),
    ss: get("second"),
  };
}

function fixedOffsetParts(ms: number, offsetMinutes: number): CivilParts {
  const d = new Date(ms + offsetMinutes * 60_000);
  return {
    y: d.getUTCFullYear(),
    m: d.getUTCMonth() + 1,
    d: d.getUTCDate(),
    hh: d.getUTCHours(),
    mm: d.getUTCMinutes(),
    ss: d.getUTCSeconds(),
  };
}

function validCivilParts(p: CivilParts): boolean {
  return [p.y, p.m, p.d, p.hh, p.mm, p.ss].every(Number.isFinite);
}

/** A timezone stored by another/newer WebView may be absent from this machine's ICU. */
function partsAtOrFallback(
  ms: number,
  timezone: string,
  fallbackOffsetMinutes: number,
): CivilParts {
  try {
    return partsAt(ms, timezone);
  } catch (e) {
    if (!(e instanceof RangeError)) throw e;
    return fixedOffsetParts(ms, fallbackOffsetMinutes);
  }
}

/** Offset của named zone tại đúng instant, gồm DST lịch sử. */
export function timezoneOffsetMinutes(timezone: string, ms: number): number {
  const p = partsAt(ms, timezone);
  const localAsUtc = Date.UTC(p.y, p.m - 1, p.d, p.hh, p.mm, p.ss);
  return Math.round((localAsUtc - Math.trunc(ms / 1000) * 1000) / 60_000);
}

export function timezoneOffsetMinutesOrFallback(
  timezone: string,
  ms: number,
  fallbackOffsetMinutes: number,
): number {
  try {
    const offset = timezoneOffsetMinutes(timezone, ms);
    return Number.isFinite(offset) ? offset : fallbackOffsetMinutes;
  } catch (e) {
    if (!(e instanceof RangeError)) throw e;
    return fallbackOffsetMinutes;
  }
}

const DAY_MS = 24 * 3_600_000;
const HOUR_MS = 3_600_000;

function utcCivilEpoch(y: number, m: number, d: number): number {
  const value = new Date(0);
  value.setUTCHours(0, 0, 0, 0);
  value.setUTCFullYear(y, m - 1, d);
  return value.getTime();
}

function civilKey(y: number, m: number, d: number): number {
  return y * 10_000 + m * 100 + d;
}

/** Find the earliest instant belonging to this local date (or to the first later date when
 * the requested date was skipped). We enumerate every offset around the date and transition
 * boundaries. Unlike a binary search on the civil date, this also works when a midnight fold
 * makes the predicate false -> true -> false -> true (St Johns/Goose Bay/Moncton). */
function localDateBoundary(y: number, m: number, d: number, timezone: string): number {
  const target = civilKey(y, m, d);
  const wall = utcCivilEpoch(y, m, d);
  const start = wall - 3 * DAY_MS;
  const end = wall + 3 * DAY_MS;
  const candidates: number[] = [];
  const seenOffsets = new Set<number>();

  const addMidnightForOffset = (offset: number) => {
    if (seenOffsets.has(offset)) return;
    seenOffsets.add(offset);
    const instant = wall - offset * 60_000;
    const p = partsAt(instant, timezone);
    if (
      civilKey(p.y, p.m, p.d) === target &&
      p.hh === 0 &&
      p.mm === 0 &&
      p.ss === 0
    ) {
      candidates.push(instant);
    }
  };

  let previousInstant = start;
  let previousOffset = timezoneOffsetMinutes(timezone, start);
  addMidnightForOffset(previousOffset);
  for (let instant = start + HOUR_MS; instant <= end; instant += HOUR_MS) {
    const offset = timezoneOffsetMinutes(timezone, instant);
    addMidnightForOffset(offset);
    if (offset !== previousOffset) {
      // Find the first millisecond whose offset differs from the preceding segment.
      let lo = previousInstant;
      let hi = instant;
      while (lo + 1 < hi) {
        const mid = lo + Math.floor((hi - lo) / 2);
        if (timezoneOffsetMinutes(timezone, mid) === previousOffset) lo = mid;
        else hi = mid;
      }
      const p = partsAt(hi, timezone);
      if (civilKey(p.y, p.m, p.d) >= target) candidates.push(hi);
    }
    previousInstant = instant;
    previousOffset = offset;
  }

  if (candidates.length === 0) {
    throw new RangeError(`Unable to resolve local date in timezone ${timezone}`);
  }
  return Math.min(...candidates);
}

/** "2026-08-03" theo zone đã chọn → epoch. End-of-day dùng midnight ngày
 * kế tiếp - 1ms nên ngày DST 23/25 giờ vẫn đúng. */
export function dateInputToEpoch(
  s: string,
  timezone: TimeZoneSpec,
  endOfDay = false,
  fallbackOffsetMinutes = 0,
): number | undefined {
  if (!s) return undefined;
  const [y, m, d] = s.split("-").map(Number);
  if (!y || !m || !d) return undefined;
  const parsed = new Date(utcCivilEpoch(y, m, d));
  if (
    parsed.getUTCFullYear() !== y ||
    parsed.getUTCMonth() + 1 !== m ||
    parsed.getUTCDate() !== d
  ) {
    return undefined;
  }
  if (typeof timezone === "number") {
    const base = utcCivilEpoch(y, m, d) - timezone * 60_000;
    return endOfDay ? base + DAY_MS - 1 : base;
  }
  try {
    const boundary = localDateBoundary(y, m, d, timezone);
    // A date skipped by a timezone reform (for example Apia 2011-12-30) has no
    // representable range. Clear the filter instead of silently changing it to yesterday.
    const atBoundary = partsAtOrFallback(boundary, timezone, fallbackOffsetMinutes);
    if (civilKey(atBoundary.y, atBoundary.m, atBoundary.d) !== civilKey(y, m, d)) {
      return undefined;
    }
    if (!endOfDay) return boundary;
    const next = new Date(utcCivilEpoch(y, m, d + 1));
    return (
      localDateBoundary(
        next.getUTCFullYear(),
        next.getUTCMonth() + 1,
        next.getUTCDate(),
        timezone,
      ) - 1
    );
  } catch (e) {
    if (!(e instanceof RangeError)) throw e;
    const base = utcCivilEpoch(y, m, d) - fallbackOffsetMinutes * 60_000;
    return endOfDay ? base + DAY_MS - 1 : base;
  }
}

export function epochToDateInput(
  ms: number | undefined,
  timezone: TimeZoneSpec,
  fallbackOffsetMinutes = 0,
): string {
  if (ms == null || !Number.isFinite(ms) || isNaN(new Date(ms).getTime())) return "";
  if (typeof timezone === "number") {
    const shifted = new Date(ms + timezone * 60_000);
    return isNaN(shifted.getTime()) ? "" : shifted.toISOString().slice(0, 10);
  }
  const p = partsAtOrFallback(ms, timezone, fallbackOffsetMinutes);
  if (!validCivilParts(p)) return "";
  const two = (n: number) => String(n).padStart(2, "0");
  return `${p.y}-${two(p.m)}-${two(p.d)}`;
}

export function fmtDateTime(
  ms: number,
  timezone: TimeZoneSpec,
  fallbackOffsetMinutes = 0,
): string {
  if (!Number.isFinite(ms) || isNaN(new Date(ms).getTime())) return "-";
  if (typeof timezone === "number") {
    const d = new Date(ms + timezone * 60_000);
    if (isNaN(d.getTime())) return "-";
    const p = (n: number) => String(n).padStart(2, "0");
    return `${d.getUTCFullYear()}-${p(d.getUTCMonth() + 1)}-${p(d.getUTCDate())} ${p(
      d.getUTCHours(),
    )}:${p(d.getUTCMinutes())}`;
  }
  const p = partsAtOrFallback(ms, timezone, fallbackOffsetMinutes);
  if (!validCivilParts(p)) return "-";
  const two = (n: number) => String(n).padStart(2, "0");
  return `${p.y}-${two(p.m)}-${two(p.d)} ${two(p.hh)}:${two(p.mm)}`;
}

export function tzOptions(extraTimezone?: string): { id: string; label: string }[] {
  const supported = (Intl as unknown as {
    supportedValuesOf?: (key: string) => string[];
  }).supportedValuesOf?.("timeZone") ?? [systemTimeZone(), "Etc/UTC"];
  const ids = [
    ...new Set([systemTimeZone(), "Etc/UTC", extraTimezone, ...supported].filter(Boolean)),
  ] as string[];
  return ids.map((id) => ({ id, label: id.replaceAll("_", " ") }));
}
