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
}

export interface QueryResult {
  queryId: number;
  total: number;
}

export interface Settings {
  setupDone: boolean;
  tzOffsetMinutes: number | null;
}

export const api = {
  addRoot: (path: string) => invoke<number>("add_root", { path }),
  listRoots: () => invoke<RootInfo[]>("list_roots"),
  removeRoot: (rootId: number) => invoke<void>("remove_root", { rootId }),
  startScan: (rootId: number) => invoke<number>("start_scan", { rootId }),
  cancelJob: (jobId: number) => invoke<boolean>("cancel_job", { jobId }),
  listJobs: () => invoke<JobRow[]>("list_jobs"),
  queryFiles: (filter: FileFilter) => invoke<QueryResult>("query_files", { filter }),
  fetchRows: (queryId: number, start: number, count: number) =>
    invoke<(FileRow | null)[]>("fetch_rows", { queryId, start, count }),
  getSettings: () => invoke<Settings>("get_settings"),
  setSettings: (tzOffsetMinutes: number, setupDone: boolean) =>
    invoke<void>("set_settings", { tzOffsetMinutes, setupDone }),
  getExcludedPaths: () => invoke<string[]>("get_excluded_paths"),
  setExcludedPaths: (paths: string[]) => invoke<void>("set_excluded_paths", { paths }),
};
