/* eslint-disable */
/**
 * Generated from the checked-in DeviceRail JSON Schema. DO NOT EDIT.
 * Run `pnpm protocol:types:generate` from the repository root.
 */

export type EventsStreamOpenResponse = EventsStreamOpenSuccessSchema | SystemHelloFailureSchema;
export type RpcIdSchema = string | number;
export type JsonRpcVersion = "2.0";
/**
 * A short-lived bearer URL. Debug intentionally never exposes its contents.
 */
export type EventStreamEndpoint = string;
/**
 * Identifies one daemon process lifetime. A cursor from another epoch must
 * never be accepted as a resumable position.
 */
export type EventStreamEpoch = string;
export type NullableRpcIdSchema = RpcIdSchema | null;

export interface EventsStreamOpenSuccessSchema {
  id: RpcIdSchema;
  jsonrpc: JsonRpcVersion;
  result: EventsStreamOpenResult;
}
export interface EventsStreamOpenResult {
  endpoint: EventStreamEndpoint;
  expiresAtMs: number;
  streamEpoch: EventStreamEpoch;
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
