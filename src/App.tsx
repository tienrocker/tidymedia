import { useEffect, useRef } from "react";
import { listen, UnlistenFn } from "@tauri-apps/api/event";
import { JobProgress } from "./lib/ipc";
import { useStore } from "./state/store";
import { FilterBar } from "./features/browse/FilterBar";
import { FileList } from "./features/browse/FileList";
import { StatusBar } from "./features/browse/StatusBar";
import { RootsPanel } from "./features/roots/RootsPanel";
import { JobsPanel } from "./features/jobs/JobsPanel";

export default function App() {
  const filter = useStore((s) => s.filter);
  const debounceRef = useRef<number | undefined>(undefined);

  // Bất kỳ thay đổi filter nào → re-query (debounce 150ms)
  useEffect(() => {
    window.clearTimeout(debounceRef.current);
    debounceRef.current = window.setTimeout(() => {
      void useStore.getState().runQuery();
    }, 150);
    return () => window.clearTimeout(debounceRef.current);
  }, [filter]);

  useEffect(() => {
    void useStore.getState().loadRoots();
    void useStore.getState().refreshJobs();

    let changedTimer: number | undefined;
    const unsubs: Promise<UnlistenFn>[] = [
      listen<JobProgress>("job://progress", (e) => {
        useStore.getState().onJobProgress(e.payload);
      }),
      listen<{ jobId: number }>("job://done", (e) => {
        useStore.getState().onJobEnd(e.payload.jobId);
        void useStore.getState().loadRoots();
        void useStore.getState().refreshJobs();
      }),
      listen<{ jobId: number }>("job://failed", (e) => {
        useStore.getState().onJobEnd(e.payload.jobId);
        void useStore.getState().refreshJobs();
      }),
      listen("index://changed", () => {
        window.clearTimeout(changedTimer);
        changedTimer = window.setTimeout(() => {
          void useStore.getState().runQuery();
          void useStore.getState().loadRoots();
        }, 300);
      }),
    ];
    return () => {
      window.clearTimeout(changedTimer);
      unsubs.forEach((p) => p.then((f) => f()));
    };
  }, []);

  return (
    <div className="flex h-screen">
      <aside className="flex w-72 shrink-0 flex-col border-r border-neutral-800 bg-neutral-950">
        <div className="px-3 py-2 text-sm font-semibold tracking-wide text-neutral-400">
          media-dedup
        </div>
        <RootsPanel />
        <div className="mt-auto">
          <JobsPanel />
        </div>
      </aside>
      <main className="flex min-w-0 flex-1 flex-col">
        <FilterBar />
        <FileList />
        <StatusBar />
      </main>
    </div>
  );
}
