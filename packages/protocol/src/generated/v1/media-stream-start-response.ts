/* eslint-disable */
/**
 * Generated from the checked-in DeviceRail JSON Schema. DO NOT EDIT.
 * Run `pnpm protocol:types:generate` from the repository root.
 */

export type MediaStreamStartResponse = MediaStreamStartSuccessSchema | SystemHelloFailureSchema;
export type RpcIdSchema = string | number;
export type JsonRpcVersion = "2.0";
export type MediaStreamKind = "screenshot" | "video";
export type NullableRpcIdSchema = RpcIdSchema | null;

export interface MediaStreamStartSuccessSchema {
  id: RpcIdSchema;
  jsonrpc: JsonRpcVersion;
  result: MediaStreamStartResult;
}
/**
 * Result returned by `media.stream.start`.
 */
export interface MediaStreamStartResult {
  stream: MediaStreamInfo;
}
export interface MediaStreamInfo {
  id: string;
  kind: MediaStreamKind;
  mediaType: string;
  viewport?: Viewport | null;
}
export interface Viewport {
  height: number;
  scaleFactor: number;
  width: number;
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
