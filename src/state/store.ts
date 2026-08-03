import { create } from "zustand";
import {
  api,
  DedupStats,
  DupGroupRow,
  DupMemberRow,
  FileFilter,
  FileRow,
  JobProgress,
  JobRow,
  RootInfo,
} from "../lib/ipc";
import { errText } from "../lib/errors";
import { systemTzOffsetMinutes } from "../lib/time";
import { fmtSize } from "../lib/format";
import i18n from "../i18n";

export const PAGE = 200;
/** LRU cap: ~50 trang (10k row) quanh viewport — không giữ cả 1M row trong heap. */
const MAX_CACHED_PAGES = 50;

// Module-level (không cần re-render): viewport hiện tại + chống toast trùng
let lastRange: [number, number] = [0, 0];
let querySeq = 0;
let toastSeq = 0;

interface ToastMsg {
  id: number;
  text: string;
  error: boolean;
}

export type ViewMode = "grid" | "list";
export type AppMode = "browse" | "dedup";
export type DedupRule = "res" | "oldest" | "newest";

/** Rule tự đánh dấu: giữ bản tốt nhất, mark xóa phần còn lại. */
export function ruleChecked(members: DupMemberRow[], rule: DedupRule): Set<number> {
  if (members.length < 2) return new Set();
  const score = (m: DupMemberRow): number => {
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

function initialViewMode(): ViewMode {
  try {
    return localStorage.getItem("viewMode") === "list" ? "list" : "grid";
  } catch {
    return "grid";
  }
}

interface AppStore {
  filter: FileFilter;
  /** Chỉ tăng khi FILTER đổi — FileList reset scroll theo cái này, KHÔNG theo queryId
   *  (index://changed lúc đang scan re-query liên tục, không được phá scroll). */
  filterEpoch: number;
  queryId: number | null;
  total: number;
  rows: Map<number, FileRow>;
  fetchedPages: Set<number>;
  pageOrder: number[];
  queryMs: number | null;
  querying: boolean;
  roots: RootInfo[];
  activeJobs: Map<number, JobProgress>;
  recentJobs: JobRow[];
  tzOffsetMinutes: number;
  setupDone: boolean;
  settingsLoaded: boolean;
  toast: ToastMsg | null;
  viewMode: ViewMode;
  /** Index (trong query hiện tại) của file đang mở lightbox; null = đóng. */
  lightboxIndex: number | null;

  appMode: AppMode;
  dupGroups: DupGroupRow[] | null;
  dupStats: DedupStats | null;
  activeGroupId: number | null;
  groupMembers: DupMemberRow[];
  /** groupId -> set fileId đánh dấu XÓA (ngữ nghĩa cố định, không đảo). */
  dupChecked: Map<number, Set<number>>;
  dedupRule: DedupRule;
  dupDeleting: boolean;

  setAppMode: (m: AppMode) => void;
  loadDupData: () => Promise<void>;
  openDupGroup: (id: number) => Promise<void>;
  toggleDupChecked: (groupId: number, fileId: number) => void;
  keepOnly: (groupId: number, fileId: number) => void;
  setDedupRule: (r: DedupRule) => void;
  deleteChecked: () => Promise<void>;

  setViewMode: (m: ViewMode) => void;
  openLightbox: (index: number) => void;
  closeLightbox: () => void;
  stepLightbox: (delta: number) => void;
  setFilter: (patch: Partial<FileFilter>) => void;
  runQuery: () => Promise<void>;
  ensureRange: (start: number, end: number) => void;
  loadRoots: () => Promise<void>;
  refreshJobs: () => Promise<void>;
  addRootAndScan: (path: string) => Promise<void>;
  removeRoot: (id: number) => Promise<void>;
  scanRoot: (id: number) => Promise<void>;
  cancelJob: (jobId: number) => Promise<void>;
  onJobProgress: (p: JobProgress) => void;
  onJobEnd: (jobId: number) => void;
  loadSettings: () => Promise<void>;
  saveSettings: (tzOffsetMinutes: number) => Promise<void>;
  showToast: (text: string, error?: boolean) => void;
}

export const useStore = create<AppStore>((set, get) => ({
  filter: {},
  filterEpoch: 0,
  queryId: null,
  total: 0,
  rows: new Map(),
  fetchedPages: new Set(),
  pageOrder: [],
  queryMs: null,
  querying: false,
  roots: [],
  activeJobs: new Map(),
  recentJobs: [],
  tzOffsetMinutes: systemTzOffsetMinutes(),
  setupDone: true, // đừng nháy wizard trước khi load xong settings
  settingsLoaded: false,
  toast: null,
  viewMode: initialViewMode(),
  lightboxIndex: null,

  appMode: "browse",
  dupGroups: null,
  dupStats: null,
  activeGroupId: null,
  groupMembers: [],
  dupChecked: new Map(),
  dedupRule: "res",

  setAppMode: (m) => {
    set({ appMode: m });
    if (m === "dedup" && get().dupGroups == null) void get().loadDupData();
  },

  loadDupData: async () => {
    try {
      const [groups, stats] = await Promise.all([api.listDupGroups(), api.dedupStats()]);
      const cur = get().activeGroupId;
      // PRUNE selection của group id không còn tồn tại — hash job rebuild làm
      // group id đổi hết; giữ lại là user xóa ngầm file họ không còn thấy.
      const valid = new Set(groups.map((g) => g.id));
      const pruned = new Map(
        [...get().dupChecked].filter(([gid]) => valid.has(gid)),
      );
      set({
        dupGroups: groups,
        dupStats: stats,
        dupChecked: pruned,
        // Nhóm đang mở biến mất sau đợt xóa/rescan → bỏ chọn
        activeGroupId: cur != null && valid.has(cur) ? cur : null,
      });
    } catch (e) {
      get().showToast(errText(e), true);
    }
  },

  openDupGroup: async (id) => {
    set({ activeGroupId: id });
    try {
      const members = await api.getDupGroup(id);
      if (get().activeGroupId !== id) return; // đã chuyển nhóm khác
      const checked = new Map(get().dupChecked);
      if (!checked.has(id)) {
        // Pre-check theo rule đang chọn — user override từng ô thoải mái
        checked.set(id, ruleChecked(members, get().dedupRule));
      }
      set({ groupMembers: members, dupChecked: checked });
    } catch (e) {
      get().showToast(errText(e), true);
    }
  },

  toggleDupChecked: (groupId, fileId) => {
    const checked = new Map(get().dupChecked);
    const set_ = new Set(checked.get(groupId) ?? []);
    if (set_.has(fileId)) {
      set_.delete(fileId);
    } else {
      // Guard cứng: không bao giờ cho check 100% member của nhóm
      const total = get().groupMembers.length;
      if (get().activeGroupId === groupId && set_.size >= total - 1) {
        get().showToast(i18n.t("dedup.keepGuard"), true);
        return;
      }
      set_.add(fileId);
    }
    checked.set(groupId, set_);
    set({ dupChecked: checked });
  },

  keepOnly: (groupId, fileId) => {
    const members = get().groupMembers;
    if (get().activeGroupId !== groupId || members.length < 2) return;
    const checked = new Map(get().dupChecked);
    checked.set(
      groupId,
      new Set(members.filter((m) => m.fileId !== fileId).map((m) => m.fileId)),
    );
    set({ dupChecked: checked });
  },

  setDedupRule: (r) => {
    set({ dedupRule: r });
    // Áp lại cho nhóm đang mở (nhóm khác giữ lựa chọn tay của user)
    const id = get().activeGroupId;
    if (id != null) {
      const checked = new Map(get().dupChecked);
      checked.set(id, ruleChecked(get().groupMembers, r));
      set({ dupChecked: checked });
    }
  },

  dupDeleting: false,

  deleteChecked: async () => {
    if (get().dupDeleting) return; // chống double-click / double-Enter
    const ids: number[] = [];
    for (const s of get().dupChecked.values()) ids.push(...s);
    if (ids.length === 0) return;
    set({ dupDeleting: true });
    let res;
    try {
      res = await api.deleteDupFiles(ids);
    } finally {
      set({ dupDeleting: false });
    }
    // Reset lựa chọn + reload mọi thứ dính tới file đã xóa
    set({ dupChecked: new Map(), groupMembers: [], activeGroupId: null });
    await get().loadDupData();
    void get().runQuery();
    void get().loadRoots();
    const t = i18n.t("dedup.deleteResult", {
      n: res.deleted,
      size: fmtSize(res.freedBytes),
    });
    get().showToast(
      res.skipped.length > 0
        ? `${t} - ${i18n.t("dedup.skipped", { n: res.skipped.length })}`
        : t,
      res.skipped.length > 0,
    );
  },

  setViewMode: (m) => {
    try {
      localStorage.setItem("viewMode", m);
    } catch {
      // private mode — thôi kệ, chỉ mất persist
    }
    set({ viewMode: m });
  },

  openLightbox: (index) => {
    set({ lightboxIndex: index });
    // Kéo sẵn hàng xóm để prev/next không trắng
    get().ensureRange(Math.max(0, index - 3), index + 3);
  },

  closeLightbox: () => set({ lightboxIndex: null }),

  stepLightbox: (delta) => {
    const { lightboxIndex, total } = get();
    if (lightboxIndex == null || total === 0) return;
    const next = Math.min(total - 1, Math.max(0, lightboxIndex + delta));
    if (next !== lightboxIndex) get().openLightbox(next);
  },

  setFilter: (patch) =>
    set({
      filter: { ...get().filter, ...patch },
      filterEpoch: get().filterEpoch + 1,
    }),

  runQuery: async () => {
    const seq = ++querySeq;
    set({ querying: true });
    try {
      const t0 = performance.now();
      const res = await api.queryFiles(get().filter);
      if (seq !== querySeq) return; // đã có query mới hơn
      const lb = get().lightboxIndex;
      set({
        queryId: res.queryId,
        total: res.total,
        rows: new Map(),
        fetchedPages: new Set(),
        pageOrder: [],
        queryMs: performance.now() - t0,
        querying: false,
        // Kết quả mới ngắn hơn vị trí đang xem → đóng lightbox thay vì trỏ bậy
        lightboxIndex: lb != null && lb >= res.total ? null : lb,
      });
    } catch (e) {
      if (seq === querySeq) set({ querying: false });
      get().showToast(errText(e), true);
    }
  },

  ensureRange: (start, end) => {
    const { queryId, total, fetchedPages } = get();
    if (queryId == null || total === 0) return;
    lastRange = [start, end];
    const first = Math.floor(Math.max(start, 0) / PAGE);
    const last = Math.floor(Math.min(end, total - 1) / PAGE);
    for (let page = first; page <= last; page++) {
      if (fetchedPages.has(page)) continue;
      fetchedPages.add(page); // in-place guard, không cần re-render

      api
        .fetchRows(queryId, page * PAGE, PAGE)
        .then((batch) => {
          // Gate theo queryId: kết quả của query cũ KHÔNG bao giờ được trộn vào
          // query mới (dữ liệu sai vị trí và không tự hồi).
          if (get().queryId !== queryId) return;
          const rows = new Map(get().rows);
          batch.forEach((r, i) => {
            if (r) rows.set(page * PAGE + i, r);
          });
          // LRU: giữ tối đa MAX_CACHED_PAGES trang, đuổi trang xa viewport trước
          let order = get().pageOrder.filter((p) => p !== page);
          order.push(page);
          if (order.length > MAX_CACHED_PAGES) {
            const [lo, hi] = lastRange;
            const far = (p: number) =>
              p * PAGE > hi + PAGE * 10 || (p + 1) * PAGE < lo - PAGE * 10;
            while (order.length > MAX_CACHED_PAGES) {
              const idx = order.findIndex(far);
              const victim = idx >= 0 ? order.splice(idx, 1)[0] : order.shift()!;
              for (let k = victim * PAGE; k < victim * PAGE + PAGE; k++) {
                rows.delete(k);
              }
              get().fetchedPages.delete(victim);
            }
          }
          set({ rows, pageOrder: order });
        })
        .catch((e) => {
          if (get().queryId !== queryId) return;
          get().fetchedPages.delete(page);
          if (String(e).includes("ERR_QUERY_EXPIRED")) {
            // Snapshot bị evict (2 generation) — re-materialize 1 lần
            void get().runQuery();
          } else {
            console.error("fetch_rows failed", e);
          }
        });
    }
  },

  loadRoots: async () => {
    try {
      set({ roots: await api.listRoots() });
    } catch (e) {
      console.error("list_roots failed", e);
    }
  },

  refreshJobs: async () => {
    try {
      set({ recentJobs: await api.listJobs() });
    } catch (e) {
      console.error("list_jobs failed", e);
    }
  },

  addRootAndScan: async (path) => {
    const rootId = await api.addRoot(path);
    await get().loadRoots();
    const jobId = await api.startScan(rootId);
    // Placeholder ngay lập tức — đừng để UI trơ tới batch đầu tiên (5k file)
    get().onJobProgress({ jobId, kind: "scan", done: 0, total: null, message: null });
  },

  removeRoot: async (id) => {
    await api.removeRoot(id);
    await get().loadRoots();
    await get().runQuery();
  },

  scanRoot: async (id) => {
    const jobId = await api.startScan(id);
    get().onJobProgress({ jobId, kind: "scan", done: 0, total: null, message: null });
  },

  cancelJob: async (jobId) => {
    await api.cancelJob(jobId);
  },

  onJobProgress: (p) => {
    const activeJobs = new Map(get().activeJobs);
    activeJobs.set(p.jobId, p);
    set({ activeJobs });
  },

  onJobEnd: (jobId) => {
    const activeJobs = new Map(get().activeJobs);
    activeJobs.delete(jobId);
    set({ activeJobs });
  },

  loadSettings: async () => {
    try {
      const s = await api.getSettings();
      set({
        setupDone: s.setupDone,
        tzOffsetMinutes: s.tzOffsetMinutes ?? systemTzOffsetMinutes(),
        settingsLoaded: true,
      });
    } catch (e) {
      console.error("get_settings failed", e);
      set({ settingsLoaded: true });
    }
  },

  saveSettings: async (tzOffsetMinutes) => {
    await api.setSettings(tzOffsetMinutes, true);
    set({ tzOffsetMinutes, setupDone: true });
  },

  showToast: (text, error = true) => {
    const id = ++toastSeq;
    set({ toast: { id, text, error } });
    window.setTimeout(() => {
      if (get().toast?.id === id) set({ toast: null });
    }, 6000);
  },
}));

/** Bọc handler async: lỗi → toast, không bao giờ unhandled rejection. */
export function runSafe(fn: () => Promise<unknown>): void {
  void fn().catch((e) => useStore.getState().showToast(errText(e), true));
}
