// src/types/common.ts
export interface ApiResponse<T = unknown> {
  success: boolean;
  message?: string;
  data?: T;
}

export interface ApiError {
  error: string | GatewayErrorDetail;
}

export interface GatewayErrorDetail {
  message: string;
  type: string;
  code: string;
}

export interface PaginatedResponse<T> {
  entries: T[];
  totalRequests: number;
  successCount: number;
}
