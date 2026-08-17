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

/** "2026-08-17" theo lịch của MÁY — đúng cái ngày user đang nghĩ là "hôm nay". */
function localDateInput(shiftDays = 0): string {
  const d = new Date();
  d.setDate(d.getDate() + shiftDays);
  const two = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${two(d.getMonth() + 1)}-${two(d.getDate())}`;
}

export function FilterBar() {
  const { t } = useTranslation();
  const filter = useStore((s) => s.filter);
  const setFilter = useStore((s) => s.setFilter);
  const cameras = useStore((s) => s.cameras);
  const tags = useStore((s) => s.tags);
  const albums = useStore((s) => s.albums);
  const tz = useStore((s) => s.timezone);
  const tzOffset = useStore((s) => s.tzOffsetMinutes);
  const viewMode = useStore((s) => s.viewMode);
  const setViewMode = useStore((s) => s.setViewMode);

  const dateField = filter.dateField ?? "taken";
  // Ngày chụp EXIF lưu dạng WALL-CLOCK (giờ trên máy ảnh, đóng gói như UTC) nên
  // quy đổi bằng offset 0; mtime là instant UTC thật nên dùng timezone của user.
  // Dùng nhầm là lệch đúng bằng offset (VN = 7 tiếng), ảnh chụp lúc 23h nhảy sang
  // hôm sau.
  const zoneOf = (field: string) => (field === "taken" ? 0 : tz);
  const fallbackOf = (field: string) => (field === "taken" ? 0 : tzOffset);

  // Giữ chuỗi ngày thô: đổi trường ngày thì NGÀY HIỂN THỊ không đổi, chỉ epoch
  // được tính lại theo quy ước mới (không tự dịch filter của user đi 7 tiếng).
  const [dateFrom, setDateFrom] = useState(() =>
    epochToDateInput(
      useStore.getState().filter.dateFrom,
      zoneOf(dateField),
      fallbackOf(dateField),
    ),
  );
  const [dateTo, setDateTo] = useState(() =>
    epochToDateInput(
      useStore.getState().filter.dateTo,
      zoneOf(dateField),
      fallbackOf(dateField),
    ),
  );

  const applyDates = (from: string, to: string, field: string) => {
    const zone = zoneOf(field);
    const fallback = fallbackOf(field);
    const epochFrom = dateInputToEpoch(from, zone, false, fallback);
    const epochTo = dateInputToEpoch(to, zone, true, fallback);
    if ((from && epochFrom == null) || (to && epochTo == null)) {
      useStore.getState().showToast(t("filter.invalidLocalDate"), true);
    }
    setFilter({
      dateFrom: epochFrom,
      dateTo: epochTo,
      dateField: field as "taken" | "mtime",
    });
  };

  // IME (Telex/Pinyin): không bắn query khi đang gõ dở tổ hợp.
  // Init từ store: component bị unmount khi sang mode dedup, quay lại phải
  // hiện đúng filter text đang hoạt động.
  const [text, setText] = useState(() => useStore.getState().filter.text ?? "");
  const composing = useRef(false);
  const pushText = (v: string) => setFilter({ text: v || undefined });

  // SizeInput giữ chuỗi gõ dở trong state RIÊNG của nó nên xoá filter ở store
  // không làm ô nhập trống theo — đổi key để React dựng lại đúng 2 ô đó.
  const [resetNonce, setResetNonce] = useState(0);

  // Khoảng ngày đi qua ĐÚNG applyDates như lúc user tự gõ: cùng một phép quy
  // đổi zone, không có đường tắt nào tính epoch kiểu khác rồi lệch 7 tiếng.
  const setRange = (from: string, to: string) => {
    setDateFrom(from);
    setDateTo(to);
    applyDates(from, to, dateField);
  };
  const thisYear = new Date().getFullYear();
  const QUICK: { key: string; from: string; to: string }[] = [
    { key: "filter.quickLast30", from: localDateInput(-30), to: "" },
    { key: "filter.quickThisYear", from: `${thisYear}-01-01`, to: "" },
    {
      key: "filter.quickLastYear",
      from: `${thisYear - 1}-01-01`,
      to: `${thisYear - 1}-12-31`,
    },
  ];
  // Chỉ tính filter THẬT SỰ thu hẹp kết quả: viewMode/sort/dateField không phải
  // là lọc, hiện nút "xoá lọc" vì chúng thì nút đó không bao giờ tắt.
  const hasFilter =
    filter.text != null ||
    filter.kind != null ||
    filter.sizeMin != null ||
    filter.sizeMax != null ||
    filter.dateFrom != null ||
    filter.dateTo != null ||
    filter.minPx != null ||
    filter.durMinMs != null ||
    filter.durMaxMs != null ||
    filter.camera != null ||
    filter.tagId != null ||
    filter.albumId != null;

  const clearAll = () => {
    setText("");
    setDateFrom("");
    setDateTo("");
    setResetNonce((n) => n + 1);
    setFilter({
      text: undefined,
      kind: undefined,
      sizeMin: undefined,
      sizeMax: undefined,
      dateFrom: undefined,
      dateTo: undefined,
      minPx: undefined,
      durMinMs: undefined,
      durMaxMs: undefined,
      camera: undefined,
      tagId: undefined,
      albumId: undefined,
      // sort "album" chỉ có nghĩa khi đang xem một album — bỏ album thì phải
      // bỏ cả cách sắp đó, không thì list im lặng quay về thứ tự ngày
      sort: filter.sort === "album" ? undefined : filter.sort,
    });
  };

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
        key={`min-${resetNonce}`}
        placeholder={t("filter.minMb")}
        initialBytes={filter.sizeMin}
        onBytes={(b) => setFilter({ sizeMin: b })}
      />
      <SizeInput
        key={`max-${resetNonce}`}
        placeholder={t("filter.maxMb")}
        initialBytes={filter.sizeMax}
        onBytes={(b) => setFilter({ sizeMax: b })}
      />
      <select
        className={inputCls}
        title={t("filter.dateField")}
        value={dateField}
        onChange={(e) => applyDates(dateFrom, dateTo, e.target.value)}
      >
        <option value="taken">{t("filter.dateTaken")}</option>
        <option value="mtime">{t("filter.dateFile")}</option>
      </select>
      <input
        type="date"
        className={inputCls}
        value={dateFrom}
        onChange={(e) => {
          setDateFrom(e.target.value);
          applyDates(e.target.value, dateTo, dateField);
        }}
      />
      <input
        type="date"
        className={inputCls}
        value={dateTo}
        onChange={(e) => {
          setDateTo(e.target.value);
          applyDates(dateFrom, e.target.value, dateField);
        }}
      />
      <select
        className={inputCls}
        value={filter.sort ?? "date_desc"}
        onChange={(e) => setFilter({ sort: e.target.value })}
      >
        {/* Chỉ có nghĩa khi đang xem một album, chỗ khác chọn vào thì lặng lẽ
            rơi về sắp theo ngày — nên chỉ hiện khi đang ở trong album */}
        {filter.albumId != null && <option value="album">{t("sort.albumOrder")}</option>}
        <option value="date_desc">{t("sort.newest")}</option>
        <option value="date_asc">{t("sort.oldest")}</option>
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
      {/* Danh sách thiết bị chỉ có sau khi job meta trích tên máy — chưa có
          thiết bị nào thì giấu hẳn ô này thay vì hiện dropdown rỗng. */}
      {cameras.length > 0 && (
        <select
          className={`${inputCls} max-w-[14rem]`}
          title={t("filter.device")}
          value={filter.camera ?? ""}
          onChange={(e) => setFilter({ camera: e.target.value || undefined })}
        >
          <option value="">{t("filter.anyDevice")}</option>
          {cameras.map((c) => (
            <option key={c.camera} value={c.camera}>
              {c.camera} ({c.count})
            </option>
          ))}
        </select>
      )}
      {tags.length > 0 && (
        <select
          className={`${inputCls} max-w-[12rem]`}
          title={t("tags.filter")}
          value={filter.tagId ?? ""}
          onChange={(e) =>
            setFilter({ tagId: e.target.value ? Number(e.target.value) : undefined })
          }
        >
          <option value="">{t("tags.any")}</option>
          {tags.map((x) => (
            <option key={x.id} value={x.id}>
              {x.name} ({x.count})
            </option>
          ))}
        </select>
      )}
      {albums.length > 0 && (
        <select
          className={`${inputCls} max-w-[12rem]`}
          title={t("albums.filter")}
          value={filter.albumId ?? ""}
          onChange={(e) => {
            const id = e.target.value ? Number(e.target.value) : undefined;
            // Mở album thì mặc định xem theo THỨ TỰ ĐÃ THÊM — đó là lý do người
            // ta xếp album; rời album thì trả lại cách sắp thường
            setFilter({
              albumId: id,
              sort: id != null ? "album" : filter.sort === "album" ? undefined : filter.sort,
            });
          }}
        >
          <option value="">{t("albums.any")}</option>
          {albums.map((a) => (
            <option key={a.id} value={a.id}>
              {a.name} ({a.count})
            </option>
          ))}
        </select>
      )}
      {QUICK.map((q) => {
        const active = dateFrom === q.from && dateTo === q.to;
        return (
          <button
            key={q.key}
            className={`rounded border px-2 py-1 text-xs ${
              active
                ? "border-emerald-700 bg-emerald-900/40 text-emerald-300"
                : "border-neutral-700 bg-neutral-900 text-neutral-400 hover:text-neutral-200"
            }`}
            // Bấm lại chip đang bật = bỏ khoảng ngày, không phải đặt lại y hệt
            onClick={() => (active ? setRange("", "") : setRange(q.from, q.to))}
          >
            {t(q.key)}
          </button>
        );
      })}
      {hasFilter && (
        <button
          className="rounded border border-neutral-700 bg-neutral-900 px-2 py-1 text-xs text-neutral-400 hover:text-neutral-200"
          onClick={clearAll}
        >
          ✕ {t("filter.clear")}
        </button>
      )}
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
