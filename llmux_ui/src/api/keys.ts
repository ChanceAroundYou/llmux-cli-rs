// src/api/keys.ts
import client from "./client";
import type {
  ApiKey,
  KeyCreatePayload,
  KeyCreateResponse,
  KeyUpdatePayload,
} from "@/types/key";

export const keyApi = {
  list: () => client.get<ApiKey[]>("/keys").then((r) => r.data),

  create: (payload: KeyCreatePayload) =>
    client.post<KeyCreateResponse>("/keys", payload).then((r) => r.data),

  update: (id: number, payload: KeyUpdatePayload) =>
    client.put(`/keys/${id}`, payload).then((r) => r.data),

  remove: (id: number) => client.delete(`/keys/${id}`).then((r) => r.data),
};
