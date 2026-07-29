/* eslint-disable */
/**
 * Generated from the checked-in DeviceRail JSON Schema. DO NOT EDIT.
 * Run `pnpm protocol:types:generate` from the repository root.
 */

export type RpcResponse = RpcSuccessSchema | RpcFailureSchema;
export type RpcIdSchema = string | number;
export type JsonRpcVersion = "2.0";
export type NullableRpcIdSchema = RpcIdSchema | null;

export interface RpcSuccessSchema {
  id: RpcIdSchema;
  jsonrpc: JsonRpcVersion;
  result: unknown;
}
export interface RpcFailureSchema {
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
