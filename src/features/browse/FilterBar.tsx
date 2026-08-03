import { useStore } from "../../state/store";

const inputCls =
  "rounded border border-neutral-700 bg-neutral-900 px-2 py-1 text-neutral-200 " +
  "placeholder-neutral-500 outline-none focus:border-neutral-500";

export function FilterBar() {
  const filter = useStore((s) => s.filter);
  const setFilter = useStore((s) => s.setFilter);

  return (
    <div className="flex flex-wrap items-center gap-2 border-b border-neutral-800 bg-neutral-950 px-3 py-2">
      <input
        className={`${inputCls} w-72`}
        placeholder="Tìm theo tên file…"
        value={filter.text ?? ""}
        onChange={(e) => setFilter({ text: e.target.value || undefined })}
        autoFocus
      />
      <select
        className={inputCls}
        value={filter.kind ?? ""}
        onChange={(e) =>
          setFilter({ kind: e.target.value === "" ? undefined : Number(e.target.value) })
        }
      >
        <option value="">Ảnh + Video</option>
        <option value="0">Ảnh</option>
        <option value="1">Video</option>
      </select>
      <input
        className={`${inputCls} w-24`}
        placeholder="≥ MB"
        inputMode="numeric"
        value={filter.sizeMin != null ? String(filter.sizeMin / (1024 * 1024)) : ""}
        onChange={(e) => {
          const v = parseFloat(e.target.value);
          setFilter({ sizeMin: isNaN(v) ? undefined : Math.round(v * 1024 * 1024) });
        }}
      />
      <input
        className={`${inputCls} w-24`}
        placeholder="≤ MB"
        inputMode="numeric"
        value={filter.sizeMax != null ? String(filter.sizeMax / (1024 * 1024)) : ""}
        onChange={(e) => {
          const v = parseFloat(e.target.value);
          setFilter({ sizeMax: isNaN(v) ? undefined : Math.round(v * 1024 * 1024) });
        }}
      />
      <input
        type="date"
        className={inputCls}
        value={filter.mtimeFrom ? new Date(filter.mtimeFrom).toISOString().slice(0, 10) : ""}
        onChange={(e) =>
          setFilter({
            mtimeFrom: e.target.value ? new Date(e.target.value).getTime() : undefined,
          })
        }
      />
      <input
        type="date"
        className={inputCls}
        value={filter.mtimeTo ? new Date(filter.mtimeTo).toISOString().slice(0, 10) : ""}
        onChange={(e) =>
          setFilter({
            mtimeTo: e.target.value
              ? new Date(e.target.value).getTime() + 24 * 3600 * 1000 - 1
              : undefined,
          })
        }
      />
      <select
        className={inputCls}
        value={filter.sort ?? "mtime_desc"}
        onChange={(e) => setFilter({ sort: e.target.value })}
      >
        <option value="mtime_desc">Mới nhất</option>
        <option value="mtime_asc">Cũ nhất</option>
        <option value="name">Tên A→Z</option>
        <option value="size_desc">Lớn nhất</option>
        <option value="size_asc">Nhỏ nhất</option>
      </select>
    </div>
  );
}
