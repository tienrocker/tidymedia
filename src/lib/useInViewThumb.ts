import { useEffect, useRef, useState } from "react";

/**
 * Cổng xin thumb kiểu Google Photos: ô phải THẬT SỰ lọt viewport (lề 25%) VÀ
 * "đậu" lại ~120ms mới bắn request. Cuộn lướt qua không tạo request nào, hàng
 * overscan ngoài tầm nhìn cũng không — viewport hiện tại luôn thắng (kết hợp
 * LIFO pool phía Rust). Trên HDD đây là khác biệt giữa "thấy ảnh ngay" và
 * "đen xì mấy phút".
 *
 * `key` = id của nội dung đang hiển thị trong ô: đổi id thì reset để ô tái sử
 * dụng (virtual list) không hiện nhầm thumb của hàng cũ.
 */
export function useInViewThumb(key: number | string | null | undefined): {
  ref: (el: HTMLElement | null) => void;
  wanted: boolean;
} {
  const boxRef = useRef<HTMLElement | null>(null);
  const [inView, setInView] = useState(false);
  const [wanted, setWanted] = useState(false);

  useEffect(() => {
    const el = boxRef.current;
    if (el == null) return;
    const io = new IntersectionObserver(
      (entries) => setInView(entries.some((en) => en.isIntersecting)),
      { rootMargin: "25%" },
    );
    io.observe(el);
    return () => io.disconnect();
    // Ô skeleton và ô thật là 2 element khác nhau → phải gắn lại observer khi
    // hàng có/mất dữ liệu.
  }, [key == null]);

  useEffect(() => {
    setWanted(false);
  }, [key]);

  useEffect(() => {
    if (key == null || !inView || wanted) return;
    const t = window.setTimeout(() => setWanted(true), 120);
    return () => window.clearTimeout(t);
  }, [key, inView, wanted]);

  return {
    ref: (el: HTMLElement | null) => {
      boxRef.current = el;
    },
    wanted,
  };
}
