// src/api/models.ts
import client from "./client";
import type {
  AvailableModelsResponse,
  AliasResponse,
  AliasCreatePayload,
  ModelTestPayload,
  ModelTestResponse,
  ModelTestAllPayload,
  ModelTestAllResponse,
  TestQueueStatus,
  ModelHealthEntry,
} from "@/types/model";

export const modelApi = {
  getAvailable: (force = false) =>
    client
      .get<AvailableModelsResponse>("/models/available", {
        params: force ? { force: "true" } : undefined,
      })
      .then((r) => r.data),

  getAliases: () =>
    client.get<AliasResponse[]>("/models/aliases").then((r) => r.data),

  createAlias: (payload: AliasCreatePayload) =>
    client.post("/models/aliases", payload).then((r) => r.data),

  deleteAlias: (id: number) =>
    client.delete(`/models/aliases/${id}`).then((r) => r.data),

  test: (payload: ModelTestPayload) =>
    client.post<ModelTestResponse>("/models/test", payload).then((r) => r.data),

  testAll: (payload: ModelTestAllPayload) =>
    client
      .post<ModelTestAllResponse>("/models/test-all", payload)
      .then((r) => r.data),

  getTestQueueStatus: () =>
    client
      .get<TestQueueStatus>("/models/test-queue/status")
      .then((r) => r.data),

  getHealth: () =>
    client.get<ModelHealthEntry[]>("/models/health").then((r) => r.data),
};
