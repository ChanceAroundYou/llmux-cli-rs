// src/hooks/usePolling.ts
import { useEffect, useRef } from "react";

export function usePolling(
  fn: () => void | Promise<void>,
  intervalMs: number,
  enabled = true,
) {
  const fnRef = useRef(fn);
  fnRef.current = fn;

  useEffect(() => {
    if (!enabled) return;
    const timer = setInterval(() => fnRef.current(), intervalMs);
    return () => clearInterval(timer);
  }, [intervalMs, enabled]);
}
