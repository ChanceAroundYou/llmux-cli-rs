// src/api/vendors.ts
import client from "./client";
import type {
  Vendor,
  VendorCreatePayload,
  VendorUpdatePayload,
} from "@/types/vendor";

export const vendorApi = {
  list: () => client.get<Vendor[]>("/vendors").then((r) => r.data),

  create: (payload: VendorCreatePayload) =>
    client.post("/vendors", payload).then((r) => r.data),

  update: (id: string, payload: VendorUpdatePayload) =>
    client.put(`/vendors/${id}`, payload).then((r) => r.data),

  remove: (id: string) => client.delete(`/vendors/${id}`).then((r) => r.data),
};
