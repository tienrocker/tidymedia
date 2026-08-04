import { useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { dateInputToEpoch, epochToDateInput } from "../../lib/time";
import { useStore } from "../../state/store";

const inputCls =
  "rounded border border-neutral-700 bg-neutral-900 px-2 py-1 text-neutral-200 " +
  "placeholder-neutral-500 outline-none focus:border-neutral-500";

const MB = 1024 * 1024;

/** Ô nhập size: giữ raw string local (gõ được "1.5"), chỉ đẩy bytes hợp lệ vào store. */
function SizeInput({
  placeholder,
  initialBytes,
  onBytes,
}: {
  placeholder: string;
  initialBytes: number | undefined;
  onBytes: (bytes: number | undefined) => void;
}) {
  // Init từ filter đang có trong store - FilterBar bị unmount khi chuyển mode
  // (dedup), quay lại mà render ô trống trong khi filter vẫn chạy = thư viện
  // "mất file" không thấy lý do.
  const [raw, setRaw] = useState(
    initialBytes != null ? String(initialBytes / MB) : "",
  );
  return (
    <input
      className={`${inputCls} w-24`}
      placeholder={placeholder}
      inputMode="decimal"
      value={raw}
      onChange={(e) => {
        const v = e.target.value;
        setRaw(v);
        if (v.trim() === "") {
          onBytes(undefined);
          return;
        }
        const f = parseFloat(v);
        if (!isNaN(f) && f >= 0) {
          onBytes(Math.round(f * MB));
        }
        // Trạng thái gõ dở ("1.", "0,") - giữ nguyên filter cũ, không phá input
      }}
    />
  );
}

export function FilterBar() {
  const { t } = useTranslation();
  const filter = useStore((s) => s.filter);
  const setFilter = useStore((s) => s.setFilter);
  const tz = useStore((s) => s.timezone);
  const tzOffset = useStore((s) => s.tzOffsetMinutes);
  const viewMode = useStore((s) => s.viewMode);
  const setViewMode = useStore((s) => s.setViewMode);

  const setDateFilter = (
    key: "mtimeFrom" | "mtimeTo",
    value: string,
    endOfDay: boolean,
  ) => {
    const epoch = dateInputToEpoch(value, tz, endOfDay, tzOffset);
    if (value && epoch == null) {
      useStore.getState().showToast(t("filter.invalidLocalDate"), true);
    }
    setFilter({ [key]: epoch });
  };

  // IME (Telex/Pinyin): không bắn query khi đang gõ dở tổ hợp.
  // Init từ store: component bị unmount khi sang mode dedup, quay lại phải
  // hiện đúng filter text đang hoạt động.
  const [text, setText] = useState(() => useStore.getState().filter.text ?? "");
  const composing = useRef(false);
  const pushText = (v: string) => setFilter({ text: v || undefined });

  return (
    <div className="flex flex-wrap items-center gap-2 border-b border-neutral-800 bg-neutral-950 px-3 py-2">
      <input
        className={`${inputCls} w-72`}
        placeholder={t("search.placeholder")}
        value={text}
        onChange={(e) => {
          setText(e.target.value);
          if (!composing.current) pushText(e.target.value);
        }}
        onCompositionStart={() => {
          composing.current = true;
        }}
        onCompositionEnd={(e) => {
          composing.current = false;
          pushText(e.currentTarget.value);
        }}
      />
      <select
        className={inputCls}
        value={filter.kind ?? ""}
        onChange={(e) =>
          setFilter({ kind: e.target.value === "" ? undefined : Number(e.target.value) })
        }
      >
        <option value="">{t("filter.all")}</option>
        <option value="0">{t("filter.images")}</option>
        <option value="1">{t("filter.videos")}</option>
      </select>
      <SizeInput
        placeholder={t("filter.minMb")}
        initialBytes={filter.sizeMin}
        onBytes={(b) => setFilter({ sizeMin: b })}
      />
      <SizeInput
        placeholder={t("filter.maxMb")}
        initialBytes={filter.sizeMax}
        onBytes={(b) => setFilter({ sizeMax: b })}
      />
      <input
        type="date"
        className={inputCls}
        value={epochToDateInput(filter.mtimeFrom, tz, tzOffset)}
        onChange={(e) => setDateFilter("mtimeFrom", e.target.value, false)}
      />
      <input
        type="date"
        className={inputCls}
        value={
          filter.mtimeTo != null ? epochToDateInput(filter.mtimeTo, tz, tzOffset) : ""
        }
        onChange={(e) => setDateFilter("mtimeTo", e.target.value, true)}
      />
      <select
        className={inputCls}
        value={filter.sort ?? "mtime_desc"}
        onChange={(e) => setFilter({ sort: e.target.value })}
      >
        <option value="mtime_desc">{t("sort.newest")}</option>
        <option value="mtime_asc">{t("sort.oldest")}</option>
        <option value="name">{t("sort.nameAz")}</option>
        <option value="size_desc">{t("sort.largest")}</option>
        <option value="size_asc">{t("sort.smallest")}</option>
      </select>
      <select
        className={inputCls}
        title={t("filter.minRes")}
        value={filter.minPx ?? ""}
        onChange={(e) =>
          setFilter({ minPx: e.target.value === "" ? undefined : Number(e.target.value) })
        }
      >
        <option value="">{t("filter.anyRes")}</option>
        <option value="2000000">≥ 2 MP</option>
        <option value="5000000">≥ 5 MP</option>
        <option value="8000000">≥ 8 MP</option>
        <option value="12000000">≥ 12 MP</option>
      </select>
      <select
        className={inputCls}
        title={t("filter.duration")}
        value={
          filter.durMinMs != null || filter.durMaxMs != null
            ? `${filter.durMinMs ?? ""}:${filter.durMaxMs ?? ""}`
            : ""
        }
        onChange={(e) => {
          const v = e.target.value;
          if (v === "") {
            setFilter({ durMinMs: undefined, durMaxMs: undefined });
            return;
          }
          const [min, max] = v.split(":");
          setFilter({
            durMinMs: min === "" ? undefined : Number(min),
            durMaxMs: max === "" ? undefined : Number(max),
          });
        }}
      >
        <option value="">{t("filter.anyDur")}</option>
        <option value=":60000">{t("filter.durShort")}</option>
        <option value="60000:600000">{t("filter.durMedium")}</option>
        <option value="600000:">{t("filter.durLong")}</option>
      </select>
      <div className="ml-auto flex overflow-hidden rounded border border-neutral-700">
        <button
          className={`px-2 py-1 text-sm ${
            viewMode === "grid"
              ? "bg-neutral-700 text-neutral-100"
              : "bg-neutral-900 text-neutral-500 hover:text-neutral-300"
          }`}
          title={t("view.grid")}
          onClick={() => setViewMode("grid")}
        >
          ▦
        </button>
        <button
          className={`px-2 py-1 text-sm ${
            viewMode === "list"
              ? "bg-neutral-700 text-neutral-100"
              : "bg-neutral-900 text-neutral-500 hover:text-neutral-300"
          }`}
          title={t("view.list")}
          onClick={() => setViewMode("list")}
        >
          ☰
        </button>
      </div>
    </div>
  );
}
