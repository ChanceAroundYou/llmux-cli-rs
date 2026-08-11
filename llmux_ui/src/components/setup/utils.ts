// 快速配置：工具函数（白名单解析、diff 生成）
import { cn } from "@/utils/helpers";

export { cn };

/** 解析密钥允许的模型：可能是 "*"、逗号串或 JSON 数组 */
export function parseAllowedModels(raw: string | string[]): string[] {
  if (!raw) return [];
  if (Array.isArray(raw)) return raw;
  if (raw === "*") return [];
  try {
    const parsed = JSON.parse(raw);
    return Array.isArray(parsed) ? parsed : [];
  } catch {
    return [];
  }
}

export interface KeyedLine {
  key: string;
  line: string;
}

/** 将嵌套对象扁平化为 "k.sub": value 的行列表，便于按 key 对比 diff */
export function flattenToLines(
  obj: Record<string, unknown>,
  prefix = "",
): KeyedLine[] {
  const result: KeyedLine[] = [];
  for (const [k, v] of Object.entries(obj)) {
    const fullKey = prefix ? `${prefix}.${k}` : k;
    if (v !== null && typeof v === "object" && !Array.isArray(v)) {
      result.push(...flattenToLines(v as Record<string, unknown>, fullKey));
    } else {
      result.push({ key: fullKey, line: `"${k}": ${JSON.stringify(v)}` });
    }
  }
  return result;
}

export type DiffLine = {
  type: "unchanged" | "removed" | "added";
  line: string;
  key?: string;
};

/** 对象级 diff：current 与 preview 按扁平 key 对比（Claude settings 预览用） */
export function buildKeyDiff(
  current: Record<string, unknown> | null,
  preview: Record<string, unknown>,
): DiffLine[] {
  const currentLines = current ? flattenToLines(current) : [];
  const previewLines = flattenToLines(preview);
  const currentMap = new Map(currentLines.map((l) => [l.key, l.line]));
  const previewMap = new Map(previewLines.map((l) => [l.key, l.line]));

  const allKeys = new Set([...currentMap.keys(), ...previewMap.keys()]);
  const result: DiffLine[] = [];
  for (const key of allKeys) {
    const cur = currentMap.get(key);
    const nxt = previewMap.get(key);
    if (cur === nxt) {
      result.push({ type: "unchanged", line: cur!, key });
    } else {
      if (cur !== undefined) result.push({ type: "removed", line: cur, key });
      if (nxt !== undefined) result.push({ type: "added", line: nxt, key });
    }
  }
  return result;
}

/** 从任意错误中提取可读信息：优先后端返回的 { error }，其次 err.message */
export function extractErrMsg(err: unknown): string {
  if (err && typeof err === "object") {
    const maybe = err as {
      response?: { data?: { error?: string } };
      message?: string;
    };
    if (typeof maybe.response?.data?.error === "string") {
      return maybe.response.data.error;
    }
    if (typeof maybe.message === "string") return maybe.message;
  }
  return String(err);
}

/** 行级 LCS diff（Codex/Gemini 的文本文件预览用） */
export function computeLineDiff(oldStr: string, newStr: string): DiffLine[] {
  const oldLines = oldStr.split("\n");
  const newLines = newStr.split("\n");
  const m = oldLines.length;
  const n = newLines.length;

  const dp: number[][] = Array.from({ length: m + 1 }, () =>
    new Array<number>(n + 1).fill(0),
  );
  for (let i = 1; i <= m; i++) {
    for (let j = 1; j <= n; j++) {
      dp[i][j] =
        oldLines[i - 1] === newLines[j - 1]
          ? dp[i - 1][j - 1] + 1
          : Math.max(dp[i - 1][j], dp[i][j - 1]);
    }
  }

  const result: DiffLine[] = [];
  let i = m;
  let j = n;
  while (i > 0 || j > 0) {
    if (i > 0 && j > 0 && oldLines[i - 1] === newLines[j - 1]) {
      result.push({ type: "unchanged", line: oldLines[i - 1] });
      i--;
      j--;
    } else if (j > 0 && (i === 0 || dp[i][j - 1] >= dp[i - 1][j])) {
      result.push({ type: "added", line: newLines[j - 1] });
      j--;
    } else {
      result.push({ type: "removed", line: oldLines[i - 1] });
      i--;
    }
  }
  return result.reverse();
}
