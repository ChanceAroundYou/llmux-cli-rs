// 快速配置：工具函数（白名单解析、diff 生成）
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
