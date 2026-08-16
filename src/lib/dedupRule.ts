// Logic THUẦN của màn Dedup: chọn bản giữ lại + trạng thái checkbox + dải
// Shift+click. Tách khỏi store để test được mà không kéo theo i18n/tauri, và
// để chỉ có DUY NHẤT một bản cài đặt rule cho cả 2 đường (mở từng nhóm và tick
// hàng loạt) - hai chỗ chấm điểm lệch nhau là đánh dấu xóa sai file.

import { DupMemberRow } from "./ipc";

export type DedupRule = "res" | "oldest" | "newest";

/** Field tối thiểu để chấm điểm — DupMemberRow lẫn DupMemberBrief đều khớp. */
export type RuleMember = Pick<
  DupMemberRow,
  "fileId" | "size" | "mtime" | "status" | "width" | "height" | "takenAt"
>;

/** Rule tự đánh dấu: giữ bản tốt nhất, mark xóa phần còn lại. */
export function ruleChecked(members: RuleMember[], rule: DedupRule): Set<number> {
  if (members.length < 2) return new Set();
  const score = (m: RuleMember): number => {
    // File không còn present không bao giờ được chọn làm bản giữ
    if (m.status !== 0) return Number.NEGATIVE_INFINITY;
    if (rule === "res") return (m.width ?? 0) * (m.height ?? 0) * 1e6 + m.size;
    const t = m.takenAt ?? m.mtime;
    return rule === "oldest" ? -t : t;
  };
  let keep = members[0];
  for (const m of members) {
    if (score(m) > score(keep)) keep = m;
  }
  return new Set(members.filter((m) => m.fileId !== keep.fileId).map((m) => m.fileId));
}

/** Trạng thái checkbox của 1 nhóm suy từ (số bản đã đánh dấu, số bản của nhóm).
 *  "all" = đã giữ đúng 1 bản; "partial" = user tự chỉnh tay dở dang. */
export function groupCheckState(
  marked: number,
  count: number,
): "none" | "partial" | "all" {
  if (marked <= 0) return "none";
  return marked >= count - 1 ? "all" : "partial";
}

/** Dải id giữa 2 lần tick (Shift+click kiểu Gmail) theo ĐÚNG thứ tự đang hiển
 *  thị. anchor không còn trong danh sách (list vừa reload) → chỉ mình id. */
export function rangeIds(
  anchor: number | null,
  id: number,
  ordered: number[],
): number[] {
  const to = ordered.indexOf(id);
  if (to < 0) return [];
  const from = anchor == null ? -1 : ordered.indexOf(anchor);
  if (from < 0) return [id];
  const [lo, hi] = from <= to ? [from, to] : [to, from];
  return ordered.slice(lo, hi + 1);
}
