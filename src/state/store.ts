import { create } from "zustand";
import {
  api,
  FileFilter,
  FileRow,
  JobProgress,
  JobRow,
  RootInfo,
} from "../lib/ipc";

export const PAGE = 200;

interface AppStore {
  filter: FileFilter;
  queryId: number | null;
  total: number;
  epoch: number;
  rows: Map<number, FileRow>;
  fetchedPages: Set<number>;
  queryMs: number | null;
  roots: RootInfo[];
  activeJobs: Map<number, JobProgress>;
  recentJobs: JobRow[];

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
}

export const useStore = create<AppStore>((set, get) => ({
  filter: {},
  queryId: null,
  total: 0,
  epoch: 0,
  rows: new Map(),
  fetchedPages: new Set(),
  queryMs: null,
  roots: [],
  activeJobs: new Map(),
  recentJobs: [],

  setFilter: (patch) => set({ filter: { ...get().filter, ...patch } }),

  runQuery: async () => {
    const epoch = get().epoch + 1;
    set({ epoch });
    const t0 = performance.now();
    try {
      const res = await api.queryFiles(get().filter);
      if (get().epoch !== epoch) return; // stale response
      set({
        queryId: res.queryId,
        total: res.total,
        rows: new Map(),
        fetchedPages: new Set(),
        queryMs: performance.now() - t0,
      });
    } catch (e) {
      console.error("query_files failed", e);
    }
  },

  ensureRange: (start, end) => {
    const { queryId, epoch, fetchedPages, total } = get();
    if (queryId == null || total === 0) return;
    const first = Math.floor(Math.max(start, 0) / PAGE);
    const last = Math.floor(Math.min(end, total - 1) / PAGE);
    for (let page = first; page <= last; page++) {
      if (fetchedPages.has(page)) continue;
      fetchedPages.add(page); // mutate in place — chỉ là guard, không cần re-render
      api
        .fetchRows(queryId, page * PAGE, PAGE)
        .then((batch) => {
          if (get().epoch !== epoch) return;
          const rows = new Map(get().rows);
          batch.forEach((r, i) => rows.set(page * PAGE + i, r));
          set({ rows });
        })
        .catch((e) => {
          console.error("fetch_rows failed", e);
          get().fetchedPages.delete(page);
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
    await api.startScan(rootId);
  },

  removeRoot: async (id) => {
    await api.removeRoot(id);
    await get().loadRoots();
    await get().runQuery();
  },

  scanRoot: async (id) => {
    await api.startScan(id);
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
}));
