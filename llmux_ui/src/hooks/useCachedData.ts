// 轻量数据缓存：模块级 Map，路由切换（组件卸载/重挂载）时跨页面保留数据。
// - 首次挂载：读缓存，命中则直接展示旧数据（无骨架）；未命中才显示骨架（延迟）。
// - 缺缓存或缓存过期：挂载后后台刷新，成功后写回缓存。
// - setData 支持函数式更新并写回缓存，保证组件内 mutation 后切走再回来数据一致。
import { useCallback, useEffect, useRef, useState } from "react";
import { useDelayedLoading } from "./useDelayedLoading";

interface CacheEntry {
  data: unknown;
  ts: number;
}

const store = new Map<string, CacheEntry>();

// 读缓存：只要存在就返回（TTL 只决定是否需要后台刷新，不丢弃旧数据展示）
function readCache<T>(key: string): T | null {
  const entry = store.get(key);
  return entry ? (entry.data as T) : null;
}

// 缓存是否新鲜：新鲜则挂载时无需后台刷新
function isFresh(key: string, ttlMs: number): boolean {
  const entry = store.get(key);
  return !!entry && Date.now() - entry.ts <= ttlMs;
}

function writeCache(key: string, data: unknown) {
  store.set(key, { data, ts: Date.now() });
}

// 手动失效指定缓存 key（数据变更类操作需要强制刷新时使用；当前页面未直接使用，预留）
export function invalidateCache(...keys: string[]) {
  keys.forEach((k) => store.delete(k));
}

interface UseCachedDataOptions {
  ttlMs?: number;
  onError?: (err: unknown) => void;
}

export function useCachedData<T>(
  key: string,
  fetcher: () => Promise<T>,
  { ttlMs = 30_000, onError }: UseCachedDataOptions = {},
) {
  // 用 ref 持有最新 fetcher/onError，避免依赖变化导致重复请求
  const fetcherRef = useRef(fetcher);
  fetcherRef.current = fetcher;
  const onErrorRef = useRef(onError);
  onErrorRef.current = onError;

  const [data, setData] = useState<T | null>(() => readCache<T>(key));
  const [loading, setLoading] = useState(() => data === null);

  const refetch = useCallback(async () => {
    setLoading(true);
    try {
      const next = await fetcherRef.current();
      writeCache(key, next);
      setData(next);
    } catch (err) {
      // 失败保留旧数据；错误交由调用方展示
      onErrorRef.current?.(err);
    } finally {
      setLoading(false);
    }
  }, [key]);

  // 缺缓存或缓存过期时触发一次后台刷新；组件重挂载时按此判断是否重新请求
  const needsFetch = data === null || !isFresh(key, ttlMs);

  useEffect(() => {
    if (needsFetch) {
      refetch();
    }
  }, [needsFetch, refetch]);

  // 更新数据并写回缓存；支持函数式更新以兼容 setState 语义
  const update = useCallback(
    (updater: T | ((prev: T | null) => T)) => {
      setData((prev) => {
        const next =
          typeof updater === "function" ? (updater as (p: T | null) => T)(prev) : updater;
        writeCache(key, next);
        return next;
      });
    },
    [key],
  );

  // 骨架显示条件：没有任何可展示数据且请求较慢（延迟阈值内完成则不闪）
  const showSkeleton = useDelayedLoading(loading && data === null, 200);

  return { data, loading, showSkeleton, setData: update, refetch };
}
