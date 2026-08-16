import { invoke } from "@tauri-apps/api/core";

export interface FileRow {
  id: number;
  name: string;
  dir: string;
  ext: string | null;
  kind: number; // 0=image 1=video
  size: number;
  mtime: number;
  status: number; // 0 present, 1 missing, 2 cloud placeholder, 3 missing-volume
  width: number | null; // null = meta job chưa chạy tới
  height: number | null;
  takenAt: number | null;
  durationMs: number | null;
  isLive: boolean; // ảnh có MOV Live Photo đi kèm (MOV ẩn khỏi list)
}

export interface FileDetail {
  id: number;
  name: string;
  dir: string;
  kind: number;
  status: number;
  size: number;
  mtime: number;
  width: number | null;
  height: number | null;
  takenAt: number | null;
  camera: string | null;
  orientation: number | null;
  durationMs: number | null;
  vcodec: string | null;
  acodec: string | null;
  fps: number | null;
  metaState: number | null;
}

export interface RootInfo {
  id: number;
  volumeId: number;
  path: string;
  lastScanAt: number | null;
  fileCount: number;
}

export interface JobRow {
  id: number;
  kind: string;
  state: string;
  done: number;
  total: number | null;
  message: string | null;
  createdAt: number | null;
  finishedAt: number | null;
  error: string | null;
}

export interface JobProgress {
  jobId: number;
  kind: string;
  done: number;
  total: number | null;
  message: string | null;
}

export interface FileFilter {
  text?: string;
  kind?: number;
  exts?: string[];
  sizeMin?: number;
  sizeMax?: number;
  mtimeFrom?: number;
  mtimeTo?: number;
  rootPath?: string;
  sort?: string;
  includeMissing?: boolean;
  minPx?: number; // width*height >= minPx (chỉ khớp file đã có meta)
  durMinMs?: number; // thời lượng video (chỉ khớp file đã có meta duration)
  durMaxMs?: number;
}

export interface QueryResult {
  queryId: number;
  total: number;
}

export interface Settings {
  setupDone: boolean;
  tzOffsetMinutes: number | null;
  timezone: string | null;
}

export interface DupGroupRow {
  id: number;
  count: number;
  size: number;
  waste: number;
  samples: [number, number][]; // [fileId, mtime] — ghép thumb URL
}

export interface DupMemberRow {
  fileId: number;
  name: string;
  dir: string;
  size: number;
  mtime: number;
  status: number;
  isLive: boolean;
  width: number | null;
  height: number | null;
  takenAt: number | null;
  camera: string | null;
}

/** Member rút gọn của MỌI nhóm — đủ để chạy rule "giữ bản nào" hàng loạt. */
export interface DupMemberBrief {
  groupId: number;
  fileId: number;
  size: number;
  mtime: number;
  status: number;
  width: number | null;
  height: number | null;
  takenAt: number | null;
}

export interface DedupStats {
  groups: number;
  waste: number;
}

export interface DeleteResult {
  deleted: number;
  freedBytes: number;
  skipped: { fileId: number; name: string; reason: string }[];
}

export interface OrgSettings {
  dirTemplate: string;
  fileTemplate: string;
  sample: string;
}

export interface LibraryRootRow {
  id: number;
  volumeId: number;
  letter: string | null;
  path: string;
  isPrimary: boolean;
}

export interface OrgPreviewRow {
  fileId: number;
  oldPath: string;
  newPath: string | null;
  action: string;
}

export interface OrgPreview {
  previewId: number;
  total: number;
  moves: number;
  copies: number;
  needsHash: number;
  skipOrganized: number;
  skipDuplicate: number;
  skipCloud: number;
  skipUncertain: number;
  skipPairBlocked: number;
  skipPathTooLong: number;
  skipOther: number;
  volumesMissingRoot: string[];
  sample: OrgPreviewRow[];
}

export interface OrgBatchRow {
  batchId: number;
  finishedAt: number | null;
  moved: number;
  undone: number;
}

export const api = {
  addRoot: (path: string) => invoke<number>("add_root", { path }),
  listRoots: () => invoke<RootInfo[]>("list_roots"),
  removeRoot: (rootId: number) => invoke<void>("remove_root", { rootId }),
  startScan: (rootId: number) => invoke<number>("start_scan", { rootId }),
  cancelJob: (jobId: number) => invoke<boolean>("cancel_job", { jobId }),
  listJobs: () => invoke<JobRow[]>("list_jobs"),
  listActiveJobs: () => invoke<JobProgress[]>("list_active_jobs"),
  queryFiles: (filter: FileFilter) => invoke<QueryResult>("query_files", { filter }),
  fetchRows: (queryId: number, start: number, count: number) =>
    invoke<(FileRow | null)[]>("fetch_rows", { queryId, start, count }),
  getSettings: () => invoke<Settings>("get_settings"),
  setSettings: (timezone: string, tzOffsetMinutes: number, setupDone: boolean) =>
    invoke<void>("set_settings", { timezone, tzOffsetMinutes, setupDone }),
  getExcludedPaths: () => invoke<string[]>("get_excluded_paths"),
  setExcludedPaths: (paths: string[]) => invoke<void>("set_excluded_paths", { paths }),
  startMetaScan: () => invoke<number | null>("start_meta_scan"),
  startThumbWarm: () => invoke<number | null>("start_thumb_warm"),
  clearThumbQueue: () => invoke<number>("clear_thumb_queue"),
  getFileMeta: (fileId: number) => invoke<FileDetail | null>("get_file_meta", { fileId }),
  openFile: (fileId: number) => invoke<void>("open_file", { fileId }),
  revealFile: (fileId: number) => invoke<void>("reveal_file", { fileId }),
  startHashScan: () => invoke<number | null>("start_hash_scan"),
  listDupGroups: () => invoke<DupGroupRow[]>("list_dup_groups"),
  getDupGroup: (groupId: number) => invoke<DupMemberRow[]>("get_dup_group", { groupId }),
  listDupMembersBrief: () => invoke<DupMemberBrief[]>("list_dup_members_brief"),
  dedupStats: () => invoke<DedupStats>("dedup_stats"),
  deleteDupFiles: (fileIds: number[]) =>
    invoke<DeleteResult>("delete_dup_files", { fileIds }),
  getOrgSettings: () => invoke<OrgSettings>("get_org_settings"),
  setOrgSettings: (dirTemplate: string, fileTemplate: string) =>
    invoke<OrgSettings>("set_org_settings", { dirTemplate, fileTemplate }),
  previewOrgTemplate: (dirTemplate: string, fileTemplate: string) =>
    invoke<string>("preview_org_template", { dirTemplate, fileTemplate }),
  listLibraryRoots: () => invoke<LibraryRootRow[]>("list_library_roots"),
  setLibraryRoot: (path: string) => invoke<number>("set_library_root", { path }),
  removeLibraryRoot: (id: number) => invoke<void>("remove_library_root", { id }),
  orgPreview: (includeUncertain: boolean) =>
    invoke<OrgPreview>("org_preview", { includeUncertain }),
  cancelOrgPreview: () => invoke<boolean>("cancel_org_preview"),
  startOrgHashScan: (includeUncertain: boolean) =>
    invoke<number | null>("start_org_hash_scan", { includeUncertain }),
  startOrganize: (includeUncertain: boolean, previewId: number) =>
    invoke<number | null>("start_organize", { includeUncertain, previewId }),
  listOrgBatches: () => invoke<OrgBatchRow[]>("list_org_batches"),
  undoOrgBatch: (batchId: number) => invoke<number | null>("undo_org_batch", { batchId }),
};
