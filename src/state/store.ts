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
import {
  LibraryRootRow,
  OrgBatchRow,
  OrgPreview,
  OrgSettings,
} from "../lib/ipc";
import { DedupRule, RuleMember, rangeIds, ruleChecked } from "../lib/dedupRule";
import { errText } from "../lib/errors";
import {
  systemTimeZone,
  systemTzOffsetMinutes,
  timezoneOffsetMinutesOrFallback,
} from "../lib/time";
import { fmtSize } from "../lib/format";
import i18n from "../i18n";

const PAGE = 200;
/** LRU cap: ~50 trang (10k row) quanh viewport - không giữ cả 1M row trong heap. */
const MAX_CACHED_PAGES = 50;

// Module-level (không cần re-render): viewport hiện tại + chống toast trùng
let lastRange: [number, number] = [0, 0];
let querySeq = 0;
let toastSeq = 0;
let orgPreviewCancelRequested = false;
/** Job id đã nhận terminal event - chặn placeholder đến muộn hồi sinh job ma. */
const endedJobIds = new Set<number>();

interface ToastMsg {
  id: number;
  text: string;
  error: boolean;
}

export type ViewMode = "grid" | "list";
export type AppMode = "browse" | "dedup" | "organize";

// Logic thuần nằm ở lib/dedupRule (test được độc lập); re-export để các
// import cũ khỏi đổi.
export type { DedupRule, RuleMember } from "../lib/dedupRule";
export { groupCheckState, rangeIds, ruleChecked } from "../lib/dedupRule";

/** Nạp MỘT lần member rút gọn của mọi nhóm (cache trong store). null = lỗi,
 *  caller không được đánh dấu gì cả. Gộp các lần gọi chồng nhau: user bấm
 *  liên tiếp trong lúc đang tải không được bắn nhiều lượt 1MB. */
let briefInFlight: Promise<Map<number, RuleMember[]> | null> | null = null;

function ensureDupBrief(
  get: () => AppStore,
  set: (partial: Partial<AppStore>) => void,
): Promise<Map<number, RuleMember[]> | null> {
  const cached = get().dupBrief;
  if (cached != null) return Promise.resolve(cached);
  if (briefInFlight != null) return briefInFlight;
  set({ dupMarking: true });
  briefInFlight = api
    .listDupMembersBrief()
    .then((rows) => {
      const byGroup = new Map<number, RuleMember[]>();
      for (const r of rows) {
        const list = byGroup.get(r.groupId);
        if (list) list.push(r);
        else byGroup.set(r.groupId, [r]);
      }
      set({ dupBrief: byGroup });
      return byGroup;
    })
    .catch((e) => {
      get().showToast(errText(e), true);
      return null;
    })
    .finally(() => {
      briefInFlight = null;
      set({ dupMarking: false });
    });
  return briefInFlight;
}

function initialViewMode(): ViewMode {
  // Mặc định LIST: hiện tức thì từ DB, không đụng đĩa — kho trên HDD chậm mà
  // mặc định grid thì màn hình đầu toàn ô đen chờ thumb. Ai thích grid bấm ▦
  // một lần là được nhớ.
  try {
    return localStorage.getItem("viewMode") === "grid" ? "grid" : "list";
  } catch {
    return "list";
  }
}

interface AppStore {
  filter: FileFilter;
  /** Chỉ tăng khi FILTER đổi - FileList reset scroll theo cái này, KHÔNG theo queryId
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
  timezone: string;
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
  /**
   * Group id mà groupMembers THUỘC VỀ (null khi đang fetch). Mọi thao tác ghi
   * dupChecked phải đối chiếu tag này với activeGroupId - không thì trong cửa
   * sổ chuyển nhóm, member của nhóm CŨ bị ghi vào checked set của nhóm MỚI
   * (P0 review: đánh dấu xóa vô hình).
   */
  groupMembersFor: number | null;
  /** groupId -> set fileId đánh dấu XÓA (ngữ nghĩa cố định, không đảo). */
  dupChecked: Map<number, Set<number>>;
  /** Cache member rút gọn của MỌI nhóm cho thao tác tick hàng loạt (nạp 1 lần,
   *  xóa mỗi lần loadDupData). Không có nó thì tick dải 500 nhóm = 500 IPC. */
  dupBrief: Map<number, RuleMember[]> | null;
  /** Nhóm của lần tick gần nhất — mốc cho Shift+click. */
  dupAnchor: number | null;
  /** Đang nạp brief / áp rule hàng loạt (disable checkbox trong lúc đó). */
  dupMarking: boolean;
  dedupRule: DedupRule;
  dupDeleting: boolean;

  orgSettings: OrgSettings | null;
  libRoots: LibraryRootRow[];
  orgPreview: OrgPreview | null;
  orgBatches: OrgBatchRow[];
  /** preview đang chạy / vừa bấm organize (disable nút) */
  orgBusy: boolean;
  orgPreviewing: boolean;
  orgIncludeUncertain: boolean;

  loadOrgData: () => Promise<void>;
  saveOrgSettings: (dirTemplate: string, fileTemplate: string) => Promise<void>;
  addLibraryRoot: (path: string) => Promise<void>;
  removeLibraryRoot: (id: number) => Promise<void>;
  setOrgIncludeUncertain: (v: boolean) => void;
  runOrgPreview: () => Promise<void>;
  cancelOrgPreview: () => Promise<void>;
  startOrgHashScan: () => Promise<void>;
  startOrganize: () => Promise<void>;
  undoOrgBatch: (batchId: number) => Promise<void>;

  setAppMode: (m: AppMode) => void;
  loadDupData: () => Promise<void>;
  openDupGroup: (id: number) => Promise<void>;
  toggleDupChecked: (groupId: number, fileId: number) => void;
  keepOnly: (groupId: number, fileId: number) => void;
  setDedupRule: (r: DedupRule) => void;
  /** Tick/bỏ tick 1 nhóm theo rule; shift = áp cho cả dải từ lần tick trước. */
  setGroupChecked: (groupId: number, checked: boolean, shift?: boolean) => Promise<void>;
  /** Tick/bỏ tick TẤT CẢ nhóm theo rule đang chọn. */
  setAllChecked: (checked: boolean) => Promise<void>;
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
  saveSettings: (timezone: string) => Promise<void>;
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
  timezone: systemTimeZone(),
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
  groupMembersFor: null,
  dupChecked: new Map(),
  dupBrief: null,
  dupAnchor: null,
  dupMarking: false,
  dedupRule: "res",

  orgSettings: null,
  libRoots: [],
  orgPreview: null,
  orgBatches: [],
  orgBusy: false,
  orgPreviewing: false,
  orgIncludeUncertain: false,

  loadOrgData: async () => {
    try {
      const [settings, roots, batches] = await Promise.all([
        api.getOrgSettings(),
        api.listLibraryRoots(),
        api.listOrgBatches(),
      ]);
      set({ orgSettings: settings, libRoots: roots, orgBatches: batches });
    } catch (e) {
      get().showToast(errText(e), true);
    }
  },

  saveOrgSettings: async (dirTemplate, fileTemplate) => {
    const settings = await api.setOrgSettings(dirTemplate, fileTemplate);
    // Template đổi → preview cũ nói dối về đích mới
    set({ orgSettings: settings, orgPreview: null });
    get().showToast(i18n.t("org.settingsSaved"), false);
  },

  addLibraryRoot: async (path) => {
    await api.setLibraryRoot(path);
    set({ libRoots: await api.listLibraryRoots(), orgPreview: null });
  },

  removeLibraryRoot: async (id) => {
    await api.removeLibraryRoot(id);
    set({ libRoots: await api.listLibraryRoots(), orgPreview: null });
  },

  setOrgIncludeUncertain: (v) => {
    // Đổi phạm vi → preview cũ hết giá trị, bắt chạy lại trước khi execute
    set({ orgIncludeUncertain: v, orgPreview: null });
  },

  runOrgPreview: async () => {
    if (get().orgBusy) return;
    orgPreviewCancelRequested = false;
    set({ orgBusy: true, orgPreviewing: true });
    try {
      const p = await api.orgPreview(get().orgIncludeUncertain);
      set({ orgPreview: p });
    } catch (e) {
      const error = String(e);
      const rejectedBeforeInvalidation =
        error.includes("ERR_INDEX_BUSY") ||
        error.includes("ERR_ORG_BUSY") ||
        error.includes("ERR_RECOVERY_BUSY");
      if (!rejectedBeforeInvalidation) {
        // Once backend preflight accepted the new preview, the old ticket is gone.
        set({ orgPreview: null });
      }
      if (
        error.includes("ERR_ORG_PREVIEW_CANCELLED") &&
        !orgPreviewCancelRequested
      ) {
        get().showToast(i18n.t("org.previewInvalidated"), false);
      } else if (!error.includes("ERR_ORG_PREVIEW_CANCELLED")) {
        get().showToast(errText(e), true);
      }
    } finally {
      orgPreviewCancelRequested = false;
      set({ orgBusy: false, orgPreviewing: false });
    }
  },

  cancelOrgPreview: async () => {
    if (!get().orgPreviewing) return;
    orgPreviewCancelRequested = true;
    await api.cancelOrgPreview();
  },

  startOrgHashScan: async () => {
    if (get().orgBusy) return;
    set({ orgBusy: true });
    try {
      const jobId = await api.startOrgHashScan(get().orgIncludeUncertain);
      if (jobId != null) {
        get().onJobProgress({
          jobId,
          kind: "org_hash",
          done: 0,
          total: null,
          message: null,
        });
        set({ orgPreview: null });
      }
    } finally {
      set({ orgBusy: false });
    }
  },

  startOrganize: async () => {
    if (get().orgBusy || get().orgPreview == null) return; // dry-run bắt buộc
    set({ orgBusy: true });
    try {
      const preview = get().orgPreview;
      if (preview == null) return;
      const jobId = await api.startOrganize(
        get().orgIncludeUncertain,
        preview.previewId,
      );
      if (jobId != null) {
        get().onJobProgress({
          jobId,
          kind: "organize",
          done: 0,
          total: null,
          message: null,
        });
      }
      set({ orgPreview: null });
    } catch (e) {
      if (String(e).includes("ERR_ORG_PREVIEW_STALE")) {
        set({ orgPreview: null });
      }
      get().showToast(errText(e), true);
    } finally {
      set({ orgBusy: false });
    }
  },

  undoOrgBatch: async (batchId) => {
    if (get().orgBusy) return;
    const jobId = await api.undoOrgBatch(batchId);
    if (jobId != null) {
      get().onJobProgress({
        jobId,
        kind: "org_undo",
        done: 0,
        total: null,
        message: null,
      });
    }
  },

  setAppMode: (m) => {
    set({ appMode: m });
    if (m === "dedup") {
      // Đang quét thì số nhóm thay đổi liên tục → vào tab là nạp lại cho tươi
      // (dup://changed chỉ nạp khi ĐANG ở tab dedup, tránh IPC thừa).
      const hashing = [...get().activeJobs.values()].some(
        (j) => j.kind === "hash" || j.kind === "org_hash",
      );
      if (get().dupGroups == null || hashing) void get().loadDupData();
    }
    if (m === "organize" && get().orgSettings == null) void get().loadOrgData();
  },

  loadDupData: async () => {
    try {
      const [groups, stats] = await Promise.all([api.listDupGroups(), api.dedupStats()]);
      const cur = get().activeGroupId;
      // PRUNE selection của group id không còn tồn tại - hash job rebuild làm
      // group id đổi hết; giữ lại là user xóa ngầm file họ không còn thấy.
      const valid = new Set(groups.map((g) => g.id));
      const pruned = new Map(
        [...get().dupChecked].filter(([gid]) => valid.has(gid)),
      );
      set({
        dupGroups: groups,
        dupStats: stats,
        dupChecked: pruned,
        // Nhóm vừa đổi (xóa xong / quét thêm) → brief cũ nói dối, nạp lại khi cần
        dupBrief: null,
        dupAnchor: null,
        // Nhóm đang mở biến mất sau đợt xóa/rescan → bỏ chọn
        activeGroupId: cur != null && valid.has(cur) ? cur : null,
      });
    } catch (e) {
      get().showToast(errText(e), true);
    }
  },

  openDupGroup: async (id) => {
    // Clear members ĐỒNG BỘ: trong cửa sổ fetch, UI không được render card
    // của nhóm cũ dưới activeGroupId mới (mọi mutation sẽ ghi nhầm nhóm).
    set({ activeGroupId: id, groupMembers: [], groupMembersFor: null });
    try {
      const members = await api.getDupGroup(id);
      if (get().activeGroupId !== id) return; // đã chuyển nhóm khác
      const checked = new Map(get().dupChecked);
      if (!checked.has(id)) {
        // Pre-check theo rule đang chọn - user override từng ô thoải mái
        checked.set(id, ruleChecked(members, get().dedupRule));
      }
      set({ groupMembers: members, groupMembersFor: id, dupChecked: checked });
    } catch (e) {
      get().showToast(errText(e), true);
    }
  },

  toggleDupChecked: (groupId, fileId) => {
    // Members chưa load xong nhóm này → không có cơ sở validate keepGuard
    if (get().groupMembersFor !== groupId) return;
    const checked = new Map(get().dupChecked);
    const set_ = new Set(checked.get(groupId) ?? []);
    if (set_.has(fileId)) {
      set_.delete(fileId);
    } else {
      // Guard cứng: không bao giờ cho check 100% member của nhóm
      const total = get().groupMembers.length;
      if (set_.size >= total - 1) {
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
    if (get().groupMembersFor !== groupId || members.length < 2) return;
    const checked = new Map(get().dupChecked);
    checked.set(
      groupId,
      new Set(members.filter((m) => m.fileId !== fileId).map((m) => m.fileId)),
    );
    set({ dupChecked: checked });
  },

  setDedupRule: (r) => {
    set({ dedupRule: r });
    const checked = new Map(get().dupChecked);
    // Áp lại cho MỌI nhóm đang được đánh dấu (brief đã nạp thì có đủ dữ liệu;
    // chưa nạp nghĩa là user chưa tick hàng loạt bao giờ).
    const brief = get().dupBrief;
    if (brief != null) {
      for (const gid of [...checked.keys()]) {
        const members = brief.get(gid);
        if (members != null) checked.set(gid, ruleChecked(members, r));
      }
    }
    // Nhóm đang mở dùng members đầy đủ - CHỈ khi chúng thuộc đúng nhóm đó
    // (đang fetch nhóm mới thì members cũ không được ghi vào nhóm mới)
    const id = get().activeGroupId;
    if (id != null && get().groupMembersFor === id) {
      checked.set(id, ruleChecked(get().groupMembers, r));
    }
    set({ dupChecked: checked });
  },

  setGroupChecked: async (groupId, checked, shift = false) => {
    const groups = get().dupGroups ?? [];
    const ordered = groups.map((g) => g.id);
    const ids = shift ? rangeIds(get().dupAnchor, groupId, ordered) : [groupId];
    if (ids.length === 0) return;
    const next = new Map(get().dupChecked);
    if (checked) {
      const brief = await ensureDupBrief(get, set);
      if (brief == null) return;
      const rule = get().dedupRule;
      for (const gid of ids) {
        const members = brief.get(gid);
        // Nhóm không còn bản nào present thì không có gì an toàn để xóa
        if (members != null) next.set(gid, ruleChecked(members, rule));
      }
    } else {
      for (const gid of ids) next.delete(gid);
    }
    set({ dupChecked: next, dupAnchor: groupId });
  },

  setAllChecked: async (checked) => {
    if (!checked) {
      set({ dupChecked: new Map(), dupAnchor: null });
      return;
    }
    const groups = get().dupGroups ?? [];
    if (groups.length === 0) return;
    const brief = await ensureDupBrief(get, set);
    if (brief == null) return;
    const rule = get().dedupRule;
    const next = new Map(get().dupChecked);
    for (const g of groups) {
      const members = brief.get(g.id);
      if (members != null) next.set(g.id, ruleChecked(members, rule));
    }
    set({ dupChecked: next, dupAnchor: null });
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
    set({
      dupChecked: new Map(),
      dupAnchor: null,
      groupMembers: [],
      activeGroupId: null,
    });
    await get().loadDupData();
    void get().runQuery();
    void get().loadRoots();
    const t = i18n.t("dedup.deleteResult", {
      n: res.deleted,
      size: fmtSize(res.freedBytes),
    });
    if (res.skipped.length > 0) {
      // Backend trả reason CODE ổn định cho từng file - gộp đếm theo lý do
      // để user biết vì sao (NO_RECYCLE_BIN trên ổ exFAT khác hẳn
      // CHANGED_ON_DISK, gộp chung 1 câu là báo sai nguyên nhân)
      const counts = new Map<string, number>();
      for (const s of res.skipped) {
        counts.set(s.reason, (counts.get(s.reason) ?? 0) + 1);
      }
      const parts = [...counts.entries()]
        .sort((a, b) => b[1] - a[1])
        .map(([r, n]) =>
          i18n.t(`dedup.reason.${r}`, { defaultValue: `${n} ${r}`, n }),
        );
      get().showToast(
        `${t} - ${i18n.t("dedup.skipped", { n: res.skipped.length })}: ${parts.join("; ")}`,
        true,
      );
    } else {
      get().showToast(t, false);
    }
  },

  setViewMode: (m) => {
    try {
      localStorage.setItem("viewMode", m);
    } catch {
      // private mode - thôi kệ, chỉ mất persist
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
            // Snapshot bị evict (2 generation) - re-materialize 1 lần
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
      const [recentJobs, activeSnapshot] = await Promise.all([
        api.listJobs(),
        api.listActiveJobs(),
      ]);
      const activeJobs = new Map(get().activeJobs);
      // Startup recovery may register before Tauri event listeners attach. Hydrate
      // running rows from DB so it is still visible/cancellable in the Jobs panel.
      for (const job of activeSnapshot) {
        if (!endedJobIds.has(job.jobId) && !activeJobs.has(job.jobId)) {
          activeJobs.set(job.jobId, job);
        }
      }
      set({ recentJobs, activeJobs });
    } catch (e) {
      console.error("list_jobs failed", e);
    }
  },

  addRootAndScan: async (path) => {
    set({ orgPreview: null });
    const rootId = await api.addRoot(path);
    await get().loadRoots();
    const jobId = await api.startScan(rootId);
    // Placeholder ngay lập tức - đừng để UI trơ tới batch đầu tiên (5k file)
    get().onJobProgress({ jobId, kind: "scan", done: 0, total: null, message: null });
  },

  removeRoot: async (id) => {
    set({ orgPreview: null });
    await api.removeRoot(id);
    await get().loadRoots();
    await get().runQuery();
  },

  scanRoot: async (id) => {
    set({ orgPreview: null });
    const jobId = await api.startScan(id);
    get().onJobProgress({ jobId, kind: "scan", done: 0, total: null, message: null });
  },

  cancelJob: async (jobId) => {
    await api.cancelJob(jobId);
  },

  onJobProgress: (p) => {
    // Tombstone: scan folder tí hin có thể bắn job://done TRƯỚC khi promise
    // startScan resolve - placeholder addRootAndScan/scanRoot đến sau sẽ
    // hồi sinh job ma quay vĩnh viễn nếu không nhớ những id đã kết thúc.
    if (endedJobIds.has(p.jobId)) return;
    const activeJobs = new Map(get().activeJobs);
    activeJobs.set(p.jobId, p);
    set({ activeJobs });
  },

  onJobEnd: (jobId) => {
    endedJobIds.add(jobId);
    if (endedJobIds.size > 500) {
      // Giữ gọn - id cũ nhất không bao giờ quay lại
      for (const id of [...endedJobIds].slice(0, 250)) endedJobIds.delete(id);
    }
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
        timezone: s.timezone ?? systemTimeZone(),
        settingsLoaded: true,
      });
    } catch (e) {
      console.error("get_settings failed", e);
      set({ settingsLoaded: true });
    }
  },

  saveSettings: async (timezone) => {
    const tzOffsetMinutes = timezoneOffsetMinutesOrFallback(
      timezone,
      Date.now(),
      get().tzOffsetMinutes,
    );
    await api.setSettings(timezone, tzOffsetMinutes, true);
    set({ timezone, tzOffsetMinutes, setupDone: true, orgPreview: null });
    // Video meta UTC đã encode theo zone cũ được backend invalidate khi đổi setting.
    void api.startMetaScan().catch((e) => get().showToast(errText(e), true));
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
