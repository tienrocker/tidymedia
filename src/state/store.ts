import { create } from "zustand";
import {
  api,
  CameraCount,
  NamedCount,
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

/** Job quét chặn lệnh xóa ở backend (nó đang ghi hash + gom lại nhóm). */
const SCAN_KINDS = ["hash", "org_hash", "phash"];

/** Tạm dừng job quét đang chạy để cú bấm xóa của user không bị từ chối, thay vì
 *  bắt user hủy cả lượt quét. Trả về id các job DO TA dừng — job user tự dừng
 *  từ trước thì không đụng tới, chạy tiếp hộ là làm sai ý họ.
 *
 *  Đây là pause JOB NỀN, không liên quan tới việc chọn/xóa: danh sách file vẫn
 *  do user tự tick và tự bấm. */
async function pauseScansForDelete(get: () => AppStore): Promise<number[]> {
  const ids = [...get().activeJobs.values()]
    .filter(
      (j) =>
        SCAN_KINDS.includes(j.kind) &&
        j.message !== "user_paused" &&
        j.message !== "user_pausing",
    )
    .map((j) => j.jobId);
  if (ids.length === 0) return [];
  await Promise.all(ids.map((id) => get().pauseJob(id, true)));
  // Cờ pause bật KHÔNG có nghĩa là luồng đã ngủ - nó có thể đang chạy nốt batch
  // dở. Xóa lúc đó vẫn an toàn (backend verify lại từng file ngay trước khi
  // đụng đĩa), nhưng chờ nó báo "đã dừng" thì tránh được việc nhóm bị gom lại
  // ngay giữa lúc xóa. Hết giờ thì cứ xóa, không treo tay user.
  const deadline = Date.now() + 20_000;
  while (Date.now() < deadline) {
    const jobs = get().activeJobs;
    const pending = ids.filter((id) => {
      const j = jobs.get(id);
      return j != null && j.message !== "user_paused";
    });
    if (pending.length === 0) break;
    await new Promise((r) => setTimeout(r, 150));
  }
  return ids;
}

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
  const kind = get().dupKind;
  briefInFlight = api
    .listDupMembersBrief(kind)
    .then((rows) => {
      if (get().dupKind !== kind) return null; // vừa đổi chế độ, dữ liệu cũ vứt đi
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
  /** Thiết bị có trong thư viện, nhiều file nhất trước. LỚN DẦN theo meta job
   *  nên phải nạp lại mỗi lần job meta xong, không cache vĩnh viễn. */
  cameras: CameraCount[];
  /** file_id đang chọn ở lưới browse. Xoá sạch mỗi lần query đổi — thao tác
   *  hàng loạt lên file KHÔNG còn nằm trong danh sách là kiểu bất ngờ tệ nhất. */
  selected: Set<number>;
  /** Index của lần chọn gần nhất, làm mốc cho shift-click. */
  selectAnchor: number | null;
  tags: NamedCount[];
  albums: NamedCount[];
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
  /** 0 = trùng tuyệt đối (byte y hệt), 1 = gần giống (cùng ảnh, khác byte). */
  dupKind: 0 | 1;
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
  setDupKind: (kind: 0 | 1) => void;
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
  /** `range` = chọn cả dải từ mốc neo tới index này (shift-click). */
  toggleSelect: (index: number, id: number, range?: boolean) => Promise<void>;
  clearSelection: () => void;
  selectAllInQuery: () => Promise<void>;
  loadCollections: () => Promise<void>;
  tagSelected: (name: string) => Promise<void>;
  addSelectedToAlbum: (albumId: number) => Promise<void>;
  loadCameras: () => Promise<void>;
  loadRoots: () => Promise<void>;
  refreshJobs: () => Promise<void>;
  addRootAndScan: (path: string) => Promise<void>;
  removeRoot: (id: number) => Promise<void>;
  scanRoot: (id: number) => Promise<void>;
  cancelJob: (jobId: number) => Promise<void>;
  pauseJob: (jobId: number, paused: boolean) => Promise<void>;
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
  cameras: [],
  selected: new Set(),
  selectAnchor: null,
  tags: [],
  albums: [],
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
  dupKind: 0,
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
        (j) => j.kind === "hash" || j.kind === "org_hash" || j.kind === "phash",
      );
      if (get().dupGroups == null || hashing) void get().loadDupData();
    }
    if (m === "organize" && get().orgSettings == null) void get().loadOrgData();
  },

  setDupKind: (kind) => {
    if (get().dupKind === kind) return;
    // Đổi loại nhóm = đổi hẳn tập dữ liệu: mọi đánh dấu/nhóm đang mở của loại
    // cũ phải bỏ, không được mang sang loại mới (id nhóm khác ngữ nghĩa).
    set({
      dupKind: kind,
      dupGroups: null,
      dupStats: null,
      dupChecked: new Map(),
      dupBrief: null,
      dupAnchor: null,
      activeGroupId: null,
      groupMembers: [],
      groupMembersFor: null,
    });
    void get().loadDupData();
  },

  loadDupData: async () => {
    const kind = get().dupKind;
    try {
      const [groups, stats] = await Promise.all([
        api.listDupGroups(kind),
        api.dedupStats(kind),
      ]);
      if (get().dupKind !== kind) return; // user vừa đổi chế độ, kết quả cũ vứt đi
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
      // Nhóm tuyệt đối được chứng minh trùng BLAKE3 nên tick sẵn là an toàn.
      // Nhóm GẦN GIỐNG chỉ là suy đoán từ hash tri giác: tick sẵn nghĩa là đặt
      // ảnh thật vào danh sách xóa trước khi user kịp nhìn. Muốn áp rule thì
      // bấm checkbox của nhóm ở cột trái (hoặc "chọn tất cả") - vẫn 1 cú bấm.
      if (!checked.has(id) && get().dupKind === 0) {
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
    // Job quét đang chạy thì backend từ chối xóa. Thay vì bắt user hủy cả lượt
    // quét, tạm dừng hộ rồi chạy tiếp — user KHÔNG mất tiến độ và cũng không
    // phải làm thêm thao tác nào. Đây là pause JOB, không đụng gì tới việc
    // chọn/xóa: ids đã do user tự tick và tự bấm.
    const paused = await pauseScansForDelete(get);
    try {
      // Hai đường xóa TÁCH HẲN nhau ở backend vì bất biến khác nhau: nhóm
      // tuyệt đối verify BLAKE3 trùng, nhóm gần giống không thể verify điều đó.
      res =
        get().dupKind === 1
          ? await api.deleteSimilarFiles(ids)
          : await api.deleteDupFiles(ids);
    } finally {
      set({ dupDeleting: false });
      for (const id of paused) void get().pauseJob(id, false);
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
        // Danh sách đổi → bỏ chọn. Giữ lại thì nút "gắn nhãn cho 200 file" sẽ
        // gắn cho những file user KHÔNG còn nhìn thấy nữa.
        selected: new Set(),
        selectAnchor: null,
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

  toggleSelect: async (index, id, range = false) => {
    const { selected, selectAnchor, queryId } = get();
    const next = new Set(selected);
    if (range && selectAnchor != null && queryId != null) {
      // Dải có thể trải qua cả phần CHƯA nạp row nào — hỏi backend lấy id,
      // không suy từ `rows` (chỉ có cửa sổ đang xem, thiếu là chọn hụt âm thầm)
      const [lo, hi] = [Math.min(selectAnchor, index), Math.max(selectAnchor, index)];
      try {
        const ids = await api.queryIdRange(queryId, lo, hi - lo + 1);
        ids.forEach((x) => next.add(x));
      } catch (e) {
        get().showToast(errText(e), true);
        return;
      }
    } else if (next.has(id)) {
      next.delete(id);
    } else {
      next.add(id);
    }
    set({ selected: next, selectAnchor: index });
  },

  clearSelection: () => set({ selected: new Set(), selectAnchor: null }),

  selectAllInQuery: async () => {
    const { queryId, total } = get();
    if (queryId == null || total === 0) return;
    try {
      const ids = await api.queryIdRange(queryId, 0, total);
      set({ selected: new Set(ids), selectAnchor: null });
    } catch (e) {
      get().showToast(errText(e), true);
    }
  },

  loadCollections: async () => {
    try {
      const [tags, albums] = await Promise.all([api.listTags(), api.listAlbums()]);
      // Nhãn/album đang lọc vừa bị xoá → bỏ filter, không để user mắc kẹt ở
      // danh sách rỗng mà không rõ vì sao
      const f = get().filter;
      const patch: Partial<FileFilter> = {};
      if (f.tagId != null && !tags.some((x) => x.id === f.tagId)) patch.tagId = undefined;
      if (f.albumId != null && !albums.some((x) => x.id === f.albumId)) {
        patch.albumId = undefined;
        if (f.sort === "album") patch.sort = undefined;
      }
      if (Object.keys(patch).length > 0) get().setFilter(patch);
      set({ tags, albums });
    } catch (e) {
      console.error("load collections failed", e);
    }
  },

  tagSelected: async (name) => {
    const ids = [...get().selected];
    if (ids.length === 0) return;
    try {
      await api.tagFiles(name, ids);
      await get().loadCollections();
      get().showToast(i18n.t("tags.tagged", { count: ids.length, name }), false);
    } catch (e) {
      get().showToast(errText(e), true);
    }
  },

  addSelectedToAlbum: async (albumId) => {
    const ids = [...get().selected];
    if (ids.length === 0) return;
    try {
      const added = await api.addToAlbum(albumId, ids);
      await get().loadCollections();
      const name = get().albums.find((a) => a.id === albumId)?.name ?? "";
      // Nói số THỰC SỰ thêm: bấm 10 file mà 7 đã nằm sẵn thì "thêm 10" là sai
      get().showToast(i18n.t("albums.added", { count: added, name }), false);
    } catch (e) {
      get().showToast(errText(e), true);
    }
  },

  loadCameras: async () => {
    try {
      const cameras = await api.listCameras();
      // Thiết bị đang lọc vừa biến mất khỏi danh sách (xoá hết ảnh của nó, gỡ
      // root) → bỏ luôn filter, không để user mắc kẹt ở list rỗng không rõ vì sao
      const cur = get().filter.camera;
      if (cur != null && !cameras.some((c) => c.camera === cur)) {
        get().setFilter({ camera: undefined });
      }
      set({ cameras });
    } catch (e) {
      console.error("list_cameras failed", e);
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

  pauseJob: async (jobId, paused) => {
    // Backend mới là nguồn sự thật: nó bắn "user_paused" khi job THẬT SỰ ngủ.
    // Job có thể đang giữa một batch dài (video vài GB) nên ở đây chỉ đánh dấu
    // "đang dừng" — nói "đã dừng" trong lúc đĩa vẫn quay là nói dối user.
    const ok = await api.pauseJob(jobId, paused);
    if (!ok) return;
    const cur = get().activeJobs.get(jobId);
    if (cur == null) return;
    const activeJobs = new Map(get().activeJobs);
    activeJobs.set(jobId, { ...cur, message: paused ? "user_pausing" : null });
    set({ activeJobs });
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
