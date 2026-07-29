/* eslint-disable */
/**
 * Generated from the checked-in DeviceRail JSON Schema. DO NOT EDIT.
 * Run `pnpm protocol:types:generate` from the repository root.
 */

export type DeviceDisconnectResponse = DeviceDisconnectSuccessSchema | SystemHelloFailureSchema;
export type RpcIdSchema = string | number;
export type JsonRpcVersion = "2.0";
export type NullableRpcIdSchema = RpcIdSchema | null;

export interface DeviceDisconnectSuccessSchema {
  id: RpcIdSchema;
  jsonrpc: JsonRpcVersion;
  result: DeviceDisconnectResult;
}
/**
 * Result returned by `device.disconnect`.
 */
export interface DeviceDisconnectResult {
  disconnected: boolean;
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
