// src/api/activity.ts
import client from "./client";
import type { ActivityResponse } from "@/types/activity";

export const activityApi = {
  list: (limit = 50) =>
    client
      .get<ActivityResponse>("/activity", { params: { limit } })
      .then((r) => r.data),
};
