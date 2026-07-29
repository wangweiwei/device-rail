/* eslint-disable */
/**
 * Generated from the checked-in DeviceRail JSON Schema. DO NOT EDIT.
 * Run `pnpm protocol:types:generate` from the repository root.
 */

export type MediaStreamCaptureResponse = MediaStreamCaptureSuccessSchema | SystemHelloFailureSchema;
export type RpcIdSchema = string | number;
export type JsonRpcVersion = "2.0";
/**
 * A one-based sequence number within one session.
 *
 * The wire value is capped at JavaScript's maximum safe integer so generated
 * clients can sort and resume event streams without losing precision.
 */
export type EventSequence = number;
export type NullableRpcIdSchema = RpcIdSchema | null;

export interface MediaStreamCaptureSuccessSchema {
  id: RpcIdSchema;
  jsonrpc: JsonRpcVersion;
  result: MediaStreamCaptureResult;
}
/**
 * Result returned by `media.stream.capture`.
 */
export interface MediaStreamCaptureResult {
  frame: MediaFrame;
}
export interface MediaFrame {
  durationMs?: number | null;
  evidence: AssetRef;
  frameIndex: EventSequence;
  keyFrame?: boolean;
  streamId: string;
}
export interface AssetRef {
  id: string;
  mediaType: string;
  sha256?: string | null;
  uri: string;
  [k: string]: unknown;
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
