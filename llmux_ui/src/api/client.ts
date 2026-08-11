// src/api/client.ts
import axios, { AxiosError } from "axios";
import type { ApiError } from "@/types/common";

const client = axios.create({
  baseURL: "/api",
  timeout: 30000,
  headers: { "Content-Type": "application/json" },
});

client.interceptors.response.use(
  (res) => res,
  (error: AxiosError<ApiError>) => {
    const message =
      typeof error.response?.data?.error === "string"
        ? error.response.data.error
        : (error.response?.data?.error?.message ?? error.message);
    return Promise.reject(new Error(message));
  },
);

export default client;
