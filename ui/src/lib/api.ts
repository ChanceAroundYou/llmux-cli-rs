export const api = (path: string): string => {
  const base = (import.meta.env.BASE_URL as string) ?? "/";
  return `${base.replace(/\/$/, "")}${path}`;
};
export const apiFetch = (path: string, init?: RequestInit) => fetch(api(path), init);
export const baseUrl = (suffix = ""): string => {
  if (typeof window === "undefined") return `http://localhost:25976${api(suffix || "/v1")}`;
  const base = (import.meta.env.BASE_URL as string) ?? "/";
  return `${window.location.origin}${base.replace(/\/$/, "")}${suffix || "/v1"}`;
};
