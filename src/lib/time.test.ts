import { describe, expect, it } from "vitest";
import {
  dateInputToEpoch,
  epochToDateInput,
  fmtDateTime,
  timezoneOffsetMinutesOrFallback,
} from "./time";

describe("IANA calendar boundaries", () => {
  it("uses the first midnight when a fold crosses midnight in St Johns", () => {
    const boundary = dateInputToEpoch("2003-10-26", "America/St_Johns");
    expect(boundary).toBe(Date.UTC(2003, 9, 26, 2, 30));

    const firstMinute = 1_067_135_430_000;
    expect(fmtDateTime(firstMinute, "America/St_Johns")).toBe(
      "2003-10-26 00:00",
    );
    expect(boundary).toBeLessThanOrEqual(firstMinute);
    expect(
      dateInputToEpoch("2003-10-25", "America/St_Johns", true),
    ).toBeLessThan(firstMinute);
  });

  it("keeps DST-short and DST-long days at 23 and 25 hours", () => {
    const springStart = dateInputToEpoch("2024-03-10", "America/New_York")!;
    const springEnd = dateInputToEpoch("2024-03-10", "America/New_York", true)!;
    expect(springEnd - springStart + 1).toBe(23 * 3_600_000);

    const fallStart = dateInputToEpoch("2024-11-03", "America/New_York")!;
    const fallEnd = dateInputToEpoch("2024-11-03", "America/New_York", true)!;
    expect(fallEnd - fallStart + 1).toBe(25 * 3_600_000);
  });

  it("uses the transition instant when local midnight is skipped", () => {
    expect(dateInputToEpoch("2014-08-01", "Africa/Cairo")).toBe(
      Date.UTC(2014, 6, 31, 22),
    );
    expect(fmtDateTime(Date.UTC(2014, 6, 31, 22), "Africa/Cairo")).toBe(
      "2014-08-01 01:00",
    );
  });

  it("rejects a skipped civil date instead of changing it to the previous day", () => {
    expect(dateInputToEpoch("2011-12-30", "Pacific/Apia")).toBeUndefined();
    expect(dateInputToEpoch("2011-12-30", "Pacific/Apia", true)).toBeUndefined();
  });

  it("falls back to the persisted numeric offset for an unknown IANA zone", () => {
    const invalid = "Mars/Olympus_Mons";
    const midnight = dateInputToEpoch("2026-08-04", invalid, false, 420);
    expect(midnight).toBe(Date.UTC(2026, 7, 3, 17));
    expect(epochToDateInput(midnight, invalid, 420)).toBe("2026-08-04");
    expect(fmtDateTime(midnight!, invalid, 420)).toBe("2026-08-04 00:00");
    expect(timezoneOffsetMinutesOrFallback(invalid, Date.now(), 420)).toBe(420);
  });

  it("contains historical end-of-day resolution failures", () => {
    for (const zone of ["Europe/Istanbul", "Europe/Sofia", "Asia/Colombo"]) {
      expect(() => dateInputToEpoch("1880-01-01", zone, true, 0)).not.toThrow();
      expect(dateInputToEpoch("1880-01-01", zone, true, 0)).toEqual(expect.any(Number));
    }
  });

  it("formats out-of-range timestamps as unavailable", () => {
    expect(fmtDateTime(1e18, "America/New_York", 0)).toBe("-");
    expect(epochToDateInput(1e18, "America/New_York", 0)).toBe("");
  });
});
