/* eslint-disable */
/**
 * Generated from the checked-in DeviceRail JSON Schema. DO NOT EDIT.
 * Run `pnpm protocol:types:generate` from the repository root.
 */

export type RequestCancelResponse = RequestCancelSuccessSchema | SystemHelloFailureSchema;
export type RpcIdSchema = string | number;
export type JsonRpcVersion = "2.0";
export type RequestCancelStatus = "requested" | "alreadyRequested" | "notFound";
export type NullableRpcIdSchema = RpcIdSchema | null;

export interface RequestCancelSuccessSchema {
  id: RpcIdSchema;
  jsonrpc: JsonRpcVersion;
  result: RequestCancelResult;
}
export interface RequestCancelResult {
  requestId: RpcIdSchema;
  status: RequestCancelStatus;
}
export interface SystemHelloFailureSchema {
  error: RpcError;
  id: NullableRpcIdSchema;
  jsonrpc: JsonRpcVersion;
}
export interface RpcError {
  code: number;
  data: ErrorInfo;
  message: string;
}
export interface ErrorInfo {
  code: string;
  details?: unknown;
  message: string;
  retryable: boolean;
}
