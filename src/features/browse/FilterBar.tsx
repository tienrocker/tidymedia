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
  onBytes,
}: {
  placeholder: string;
  onBytes: (bytes: number | undefined) => void;
}) {
  const [raw, setRaw] = useState("");
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
        // Trạng thái gõ dở ("1.", "0,") — giữ nguyên filter cũ, không phá input
      }}
    />
  );
}

export function FilterBar() {
  const { t } = useTranslation();
  const filter = useStore((s) => s.filter);
  const setFilter = useStore((s) => s.setFilter);
  const tz = useStore((s) => s.tzOffsetMinutes);

  // IME (Telex/Pinyin): không bắn query khi đang gõ dở tổ hợp
  const [text, setText] = useState("");
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
        autoFocus
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
        onBytes={(b) => setFilter({ sizeMin: b })}
      />
      <SizeInput
        placeholder={t("filter.maxMb")}
        onBytes={(b) => setFilter({ sizeMax: b })}
      />
      <input
        type="date"
        className={inputCls}
        value={epochToDateInput(filter.mtimeFrom, tz)}
        onChange={(e) => setFilter({ mtimeFrom: dateInputToEpoch(e.target.value, tz) })}
      />
      <input
        type="date"
        className={inputCls}
        value={
          filter.mtimeTo != null ? epochToDateInput(filter.mtimeTo - 1, tz) : ""
        }
        onChange={(e) =>
          setFilter({ mtimeTo: dateInputToEpoch(e.target.value, tz, true) })
        }
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
    </div>
  );
}
