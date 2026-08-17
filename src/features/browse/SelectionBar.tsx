import { useState } from "react";
import { useTranslation } from "react-i18next";
import { api } from "../../lib/ipc";
import { errText } from "../../lib/errors";
import { useStore } from "../../state/store";

const inputCls =
  "rounded border border-neutral-700 bg-neutral-900 px-2 py-1 text-neutral-200 " +
  "placeholder-neutral-500 outline-none focus:border-neutral-500";
const btnCls =
  "rounded border border-neutral-700 bg-neutral-900 px-2 py-1 text-xs " +
  "text-neutral-300 hover:bg-neutral-800 disabled:opacity-40";

/** Thanh thao tác hàng loạt, chỉ hiện khi đang chọn file.
 *
 * Không có nút XOÁ ở đây: xoá file đi qua tab Trùng lặp, nơi có đủ ngữ cảnh
 * "còn bản nào khác không". Một nút xoá cạnh nút gắn nhãn là quá dễ bấm nhầm.
 */
export function SelectionBar() {
  const { t } = useTranslation();
  const selected = useStore((s) => s.selected);
  const total = useStore((s) => s.total);
  const albums = useStore((s) => s.albums);
  const tags = useStore((s) => s.tags);
  const [tagName, setTagName] = useState("");
  const [busy, setBusy] = useState(false);

  if (selected.size === 0) return null;

  const run = async (fn: () => Promise<void>) => {
    setBusy(true);
    try {
      await fn();
    } finally {
      setBusy(false);
    }
  };

  const applyTag = () =>
    run(async () => {
      const name = tagName.trim();
      if (!name) return;
      await useStore.getState().tagSelected(name);
      setTagName("");
    });

  const newAlbum = () =>
    run(async () => {
      const name = window.prompt(t("albums.newPrompt"))?.trim();
      if (!name) return;
      try {
        const id = await api.createAlbum(name);
        await useStore.getState().loadCollections();
        await useStore.getState().addSelectedToAlbum(id);
      } catch (e) {
        useStore.getState().showToast(errText(e), true);
      }
    });

  return (
    <div className="flex flex-wrap items-center gap-2 border-b border-neutral-800 bg-emerald-950/30 px-3 py-2 text-sm">
      <span className="text-neutral-200">
        {t("select.count", { n: selected.size })}
      </span>
      {selected.size < total && (
        <button
          className={btnCls}
          onClick={() => run(() => useStore.getState().selectAllInQuery())}
        >
          {t("select.all", { n: total })}
        </button>
      )}
      <button className={btnCls} onClick={() => useStore.getState().clearSelection()}>
        {t("select.clear")}
      </button>

      <span className="ml-2 text-neutral-500">|</span>
      {/* datalist gợi ý nhãn đã có nhưng VẪN gõ tên mới được — nhãn mới là
          trường hợp thường gặp nhất, ép chọn từ danh sách là bắt user tạo trước */}
      <input
        className={`${inputCls} w-40 text-xs`}
        list="tag-suggestions"
        placeholder={t("tags.placeholder")}
        value={tagName}
        onChange={(e) => setTagName(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter") void applyTag();
        }}
      />
      <datalist id="tag-suggestions">
        {tags.map((x) => (
          <option key={x.id} value={x.name} />
        ))}
      </datalist>
      <button className={btnCls} disabled={busy || !tagName.trim()} onClick={applyTag}>
        {t("tags.apply")}
      </button>

      <span className="ml-2 text-neutral-500">|</span>
      <select
        className={`${inputCls} max-w-[12rem] text-xs`}
        value=""
        disabled={busy || albums.length === 0}
        onChange={(e) => {
          const id = Number(e.target.value);
          if (id) void run(() => useStore.getState().addSelectedToAlbum(id));
        }}
      >
        <option value="">{t("albums.addTo")}</option>
        {albums.map((a) => (
          <option key={a.id} value={a.id}>
            {a.name} ({a.count})
          </option>
        ))}
      </select>
      <button className={btnCls} disabled={busy} onClick={newAlbum}>
        + {t("albums.new")}
      </button>
    </div>
  );
}
