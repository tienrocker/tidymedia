// URL cho custom protocol (Windows/WebView2: http://<scheme>.localhost/…).
// Luôn kèm ?v=<mtime> — response phía Rust là immutable, file đổi thì URL đổi.

export const THUMB_GRID = 256;
export const THUMB_PREVIEW = 1600;

export function thumbUrl(id: number, s: number, mtime: number): string {
  return `http://thumb.localhost/${id}?s=${s}&v=${mtime}`;
}

export function mediaUrl(id: number, mtime: number): string {
  return `http://media.localhost/${id}?v=${mtime}`;
}

/** Format WebView2 (Chromium) render trực tiếp được — lightbox dùng bản gốc.
 *  Còn lại (heic/tif/raw…) dùng thumb 1600 do backend decode. */
const NATIVE_EXTS = new Set([
  "jpg",
  "jpeg",
  "jfif",
  "png",
  "gif",
  "webp",
  "bmp",
  "avif",
  "ico",
]);

export function canNativeDisplay(ext: string | null): boolean {
  return ext != null && NATIVE_EXTS.has(ext);
}
