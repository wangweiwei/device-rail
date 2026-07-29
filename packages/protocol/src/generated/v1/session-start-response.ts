/* eslint-disable */
/**
 * Generated from the checked-in DeviceRail JSON Schema. DO NOT EDIT.
 * Run `pnpm protocol:types:generate` from the repository root.
 */

export type SessionStartResponse = SessionStartSuccessSchema | SystemHelloFailureSchema;
export type RpcIdSchema = string | number;
export type JsonRpcVersion = "2.0";
/**
 * A one-based sequence number within one session.
 *
 * The wire value is capped at JavaScript's maximum safe integer so generated
 * clients can sort and resume event streams without losing precision.
 */
export type EventSequence = number;
export type SessionState = "active" | "ended";
export type NullableRpcIdSchema = RpcIdSchema | null;

export interface SessionStartSuccessSchema {
  id: RpcIdSchema;
  jsonrpc: JsonRpcVersion;
  result: SessionInfo;
}
export interface SessionInfo {
  endedAtMs?: number | null;
  eventCount: EventSequence;
  id: string;
  lastSequence: EventSequence;
  startedAtMs: number;
  state: SessionState;
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
