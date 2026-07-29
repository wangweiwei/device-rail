/* eslint-disable */
/**
 * Generated from the checked-in DeviceRail JSON Schema. DO NOT EDIT.
 * Run `pnpm protocol:types:generate` from the repository root.
 */

export type EventsClearResponse = EventsClearSuccessSchema | SystemHelloFailureSchema;
export type RpcIdSchema = string | number;
export type JsonRpcVersion = "2.0";
export type NullableRpcIdSchema = RpcIdSchema | null;

export interface EventsClearSuccessSchema {
  id: RpcIdSchema;
  jsonrpc: JsonRpcVersion;
  result: EventsClearResult;
}
/**
 * Result returned by `events.clear`.
 */
export interface EventsClearResult {
  deleted: boolean;
  sessionId: string;
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
