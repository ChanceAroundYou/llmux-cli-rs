// src/api/accounts.ts
import client from "./client";
import type {
  AccountPublic,
  AccountCreatePayload,
  AccountCreateResponse,
  AccountUpdatePayload,
  AccountUpdateResponse,
  AccountDeleteResponse,
} from "@/types/account";

export const accountApi = {
  list: () => client.get<AccountPublic[]>("/accounts").then((r) => r.data),

  create: (payload: AccountCreatePayload) =>
    client
      .post<AccountCreateResponse>("/accounts", payload)
      .then((r) => r.data),

  update: (id: number, payload: AccountUpdatePayload) =>
    client
      .put<AccountUpdateResponse>(`/accounts/${id}`, payload)
      .then((r) => r.data),

  remove: (id: number) =>
    client.delete<AccountDeleteResponse>(`/accounts/${id}`).then((r) => r.data),

  exportCsv: (id: number) =>
    client.get(`/accounts/${id}/export`, { responseType: "blob" }).then((r) => {
      const url = URL.createObjectURL(r.data);
      const a = document.createElement("a");
      a.href = url;
      a.download = `usage_history_account_${id}.csv`;
      a.click();
      URL.revokeObjectURL(url);
    }),
};
