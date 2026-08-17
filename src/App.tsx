import { useEffect, useRef, useState } from "react";
import { listen, UnlistenFn } from "@tauri-apps/api/event";
import { useTranslation } from "react-i18next";
import { LANGS } from "./i18n";
import { errText } from "./lib/errors";
import { api, JobProgress } from "./lib/ipc";
import { useStore } from "./state/store";
import { FilterBar } from "./features/browse/FilterBar";
import { FileList } from "./features/browse/FileList";
import { FileGrid } from "./features/browse/FileGrid";
import { Lightbox } from "./features/browse/Lightbox";
import { StatusBar } from "./features/browse/StatusBar";
import { DedupView } from "./features/dedup/DedupView";
import { OrganizeView } from "./features/organize/OrganizeView";
import { RootsPanel } from "./features/roots/RootsPanel";
import { JobsPanel } from "./features/jobs/JobsPanel";
import { SettingsDialog } from "./features/setup/SetupWizard";

function Toast() {
  const toast = useStore((s) => s.toast);
  if (!toast) return null;
  return (
    <div
      className={`fixed bottom-4 right-4 z-50 max-w-md rounded border px-3 py-2 text-sm shadow-lg ${
        toast.error
          ? "border-red-800 bg-red-950 text-red-200"
          : "border-neutral-700 bg-neutral-900 text-neutral-200"
      }`}
    >
      {toast.text}
    </div>
  );
}

export default function App() {
  const { t, i18n } = useTranslation();
  const filter = useStore((s) => s.filter);
  const setupDone = useStore((s) => s.setupDone);
  const settingsLoaded = useStore((s) => s.settingsLoaded);
  const viewMode = useStore((s) => s.viewMode);
  const appMode = useStore((s) => s.appMode);
  const [showSettings, setShowSettings] = useState(false);
  const debounceRef = useRef<number | undefined>(undefined);

  // Filter đổi → re-query (debounce 150ms)
  useEffect(() => {
    window.clearTimeout(debounceRef.current);
    debounceRef.current = window.setTimeout(() => {
      void useStore.getState().runQuery();
    }, 150);
    return () => window.clearTimeout(debounceRef.current);
  }, [filter]);

  useEffect(() => {
    void useStore.getState().loadSettings();
    void useStore.getState().loadRoots();
    void useStore.getState().loadCameras();
    void useStore.getState().refreshJobs();
    // Ảnh index rồi mà chưa có meta (app tắt giữa chừng...) → chạy nốt.
    // Không còn gì pending thì backend trả null, không tạo job rác.
    let metaRetryTimer: number | undefined;
    let startInitialMeta = () => {
      api.startMetaScan().catch((e) => {
        if (String(e).includes("ERR_RECOVERY_BUSY")) {
          void useStore.getState().refreshJobs();
          metaRetryTimer = window.setTimeout(startInitialMeta, 500);
        } else {
          console.error("start_meta_scan failed", e);
        }
      });
    };
    startInitialMeta();
    // Warm thumb nền (ưu tiên thấp nhất, tự nhường mọi thứ) — backend tự
    // bỏ qua nếu đang chạy/không còn gì; recovery đang bận thì thôi, lượt
    // meta xong sẽ kích lại.
    api.startThumbWarm().catch(() => {});

    // index://changed: Rust đã giãn 2.5s khi đang scan; debounce JS 1500ms nữa
    // để gom event done+changed. Re-query GIỮ scroll (không đụng filterEpoch).
    let changedTimer: number | undefined;
    const unsubs: Promise<UnlistenFn>[] = [
      listen<JobProgress>("job://progress", (e) => {
        useStore.getState().onJobProgress(e.payload);
      }),
      listen<{ jobId: number; kind?: string; message?: string }>("job://done", (e) => {
        useStore.getState().onJobEnd(e.payload.jobId);
        void useStore.getState().loadRoots();
        void useStore.getState().refreshJobs();
        const cancelled = e.payload.message === "cancelled";
        if (cancelled) {
          useStore.getState().showToast(t("jobs.cancelled"), false);
        }
        // Scan xong → trích meta cho file mới (idempotent, đang chạy thì thôi)
        if (e.payload.kind === "scan") {
          api.startMetaScan().catch((err) => console.error("start_meta_scan failed", err));
        }
        // Meta xong → warm thumb nền cho file mới (ưu tiên thấp nhất), và
        // danh sách thiết bị vừa đầy thêm (tên máy chỉ biết được sau khi trích)
        if (e.payload.kind === "meta") {
          api.startThumbWarm().catch((err) => console.error("start_thumb_warm failed", err));
          void useStore.getState().loadCameras();
        }
        // Quét hash xong → refresh danh sách nhóm trùng
        if (e.payload.kind === "hash" || e.payload.kind === "org_hash") {
          void useStore.getState().loadDupData();
        }
        if (e.payload.kind === "org_hash") {
          void useStore.getState().loadOrgData();
          if (!cancelled) {
            useStore.getState().showToast(t("org.hashPreparedDone"), false);
          }
        }
        // Organize/undo xong → path đổi hàng loạt: refresh browse + batches
        if (e.payload.kind === "organize" || e.payload.kind === "org_undo") {
          void useStore.getState().runQuery();
          void useStore.getState().loadOrgData();
          if (e.payload.message && !cancelled) {
            useStore.getState().showToast(e.payload.message, false);
          }
        }
      }),
      listen<{ jobId: number; error?: string }>("job://failed", (e) => {
        useStore.getState().onJobEnd(e.payload.jobId);
        void useStore.getState().refreshJobs();
        useStore.getState().showToast(errText(e.payload.error ?? "ERR_INTERNAL|"), true);
      }),
      // Nhóm trùng vừa được gom lại giữa chừng job hash — chỉ nạp khi user
      // đang ở tab Dedup (backend đã throttle 8s nên không cần debounce nữa).
      listen<{ kind: number }>("dup://changed", (e) => {
        const st = useStore.getState();
        if (st.appMode !== "dedup") return;
        // Job phash gom nhóm gần giống trong lúc user đang xem tab tuyệt đối
        // (và ngược lại) — nạp lại là vứt tick của tab đang mở mà chẳng đổi gì.
        if (e.payload.kind !== st.dupKind) return;
        void st.loadDupData();
      }),
      listen("index://changed", () => {
        // Dry-run organize là snapshot. Bất kỳ thay đổi index/meta/hash nào
        // cũng làm ticket cũ hết hiệu lực trước khi user bấm Execute.
        const state = useStore.getState();
        if (state.orgPreview != null && !state.orgPreviewing) {
          state.showToast(t("org.previewInvalidated"), false);
        }
        useStore.setState({ orgPreview: null });
        window.clearTimeout(changedTimer);
        changedTimer = window.setTimeout(() => {
          void useStore.getState().runQuery();
          void useStore.getState().loadRoots();
        }, 1500);
      }),
    ];
    return () => {
      window.clearTimeout(metaRetryTimer);
      window.clearTimeout(changedTimer);
      unsubs.forEach((p) => p.then((f) => f()).catch(() => {}));
    };
  }, []);

  return (
    <div className="flex h-screen">
      <aside className="flex w-72 shrink-0 flex-col border-r border-neutral-800 bg-neutral-950">
        <div className="flex items-center justify-between px-3 py-2">
          <span className="text-sm font-semibold tracking-wide text-neutral-400">
            {t("app.name")}
          </span>
          <div className="flex items-center gap-1">
            <select
              className="rounded border border-neutral-800 bg-neutral-900 px-1 py-0.5 text-xs text-neutral-400 outline-none"
              value={i18n.resolvedLanguage ?? "en"}
              onChange={(e) => void i18n.changeLanguage(e.target.value)}
            >
              {LANGS.map((l) => (
                <option key={l} value={l}>
                  {l.toUpperCase()}
                </option>
              ))}
            </select>
            <button
              title={t("wizard.settingsTitle")}
              onClick={() => setShowSettings(true)}
              className="rounded border border-neutral-800 bg-neutral-900 px-1.5 py-0.5 text-xs text-neutral-400 hover:text-neutral-200"
            >
              ⚙
            </button>
          </div>
        </div>
        {/* Mode: Browse / Dedup / Organize */}
        <div className="flex gap-1 px-3 pb-2">
          {(
            [
              ["browse", "🖼"],
              ["dedup", "♻"],
              ["organize", "🗂"],
            ] as const
          ).map(([m, icon]) => (
            <button
              key={m}
              className={`flex-1 rounded px-2 py-1 text-xs font-medium ${
                appMode === m
                  ? "bg-neutral-700 text-neutral-100"
                  : "bg-neutral-900 text-neutral-500 hover:text-neutral-300"
              }`}
              onClick={() => useStore.getState().setAppMode(m)}
            >
              {icon} {t(`mode.${m}`)}
            </button>
          ))}
        </div>
        <RootsPanel />
        <div className="mt-auto">
          <JobsPanel />
        </div>
      </aside>
      <main className="flex min-w-0 flex-1 flex-col">
        {appMode === "dedup" ? (
          <DedupView />
        ) : appMode === "organize" ? (
          <OrganizeView />
        ) : (
          <>
            <FilterBar />
            {viewMode === "grid" ? <FileGrid /> : <FileList />}
            <StatusBar />
          </>
        )}
      </main>
      <Lightbox />
      {settingsLoaded && !setupDone && <SettingsDialog firstRun />}
      {showSettings && (
        <SettingsDialog firstRun={false} onClose={() => setShowSettings(false)} />
      )}
      <Toast />
    </div>
  );
}
