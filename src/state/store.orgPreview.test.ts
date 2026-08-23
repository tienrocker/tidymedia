import { describe, expect, it, vi } from "vitest";

// Store kéo cả tầng IPC (@tauri-apps/api) và i18n vào — mock cả hai, test này
// chỉ quan tâm máy trạng thái preview, không quan tâm dây dẫn.
vi.mock("../lib/ipc", () => ({
  api: {
    orgPreview: vi.fn(),
  },
}));
vi.mock("../i18n", () => ({ default: { t: (k: string) => k } }));

import { api } from "../lib/ipc";
import { useStore } from "./store";

const fakePreview = { previewId: 7 } as never;

describe("runOrgPreview vs đổi cấu hình trong lúc chờ", () => {
  it("response về muộn KHÔNG được cài đè preview đã bị setter xóa", async () => {
    let resolvePreview!: (v: unknown) => void;
    vi.mocked(api.orgPreview).mockReturnValue(
      new Promise((r) => {
        resolvePreview = r;
      }) as never,
    );

    const inflight = useStore.getState().runOrgPreview();
    // User đổi cấu hình khi IPC còn đang bay: preview bị vứt, đời tăng.
    useStore.getState().setOrgIncludeUncertain(true);

    resolvePreview(fakePreview);
    await inflight;

    // Backend cũng đã từ chối ticket đó — UI mà cài lại là hiện số lượng và
    // nút Gom của cấu hình cũ.
    expect(useStore.getState().orgPreview).toBeNull();
    expect(useStore.getState().orgBusy).toBe(false);
  });

  it("không ai đụng cấu hình thì response được cài như thường", async () => {
    vi.mocked(api.orgPreview).mockResolvedValue(fakePreview);

    await useStore.getState().runOrgPreview();

    expect(useStore.getState().orgPreview).toEqual(fakePreview);
  });
});
