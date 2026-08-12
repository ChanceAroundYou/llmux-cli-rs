// 延迟 loading 显示：请求在 delay（默认 200ms）内完成时始终不渲染骨架，
// 只有请求确实较慢（超过阈值）才显示骨架，避免本地后端快速响应时的"闪一下"。
import { useEffect, useState } from "react";

export function useDelayedLoading(raw: boolean, delay = 200): boolean {
  const [show, setShow] = useState(false);

  useEffect(() => {
    if (!raw) {
      // 请求结束：立即隐藏，避免残留骨架
      setShow(false);
      return;
    }
    // 请求进行中：延迟 delay 后才显示骨架，期间完成则不会闪
    const timer = setTimeout(() => setShow(true), delay);
    return () => clearTimeout(timer);
  }, [raw, delay]);

  return show;
}
