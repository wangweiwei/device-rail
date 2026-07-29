/* eslint-disable */
/**
 * Generated from the checked-in DeviceRail JSON Schema. DO NOT EDIT.
 * Run `pnpm protocol:types:generate` from the repository root.
 */

export type RpcIdSchema = string | number;
export type JsonRpcVersion = "2.0";
export type MediaStreamCaptureMethodSchema = "media.stream.capture";
/**
 * A positive timeout in milliseconds that is safe to represent as a JSON
 * number in every supported client language.
 */
export type RequestTimeoutMs = number;

export interface MediaStreamCaptureRequest {
  id: RpcIdSchema;
  jsonrpc: JsonRpcVersion;
  method: MediaStreamCaptureMethodSchema;
  params: MediaStreamCaptureParams;
  timeoutMs?: RequestTimeoutMs;
}
/**
 * Parameters accepted by `media.stream.capture`.
 */
export interface MediaStreamCaptureParams {
  durationMs?: number | null;
  /**
   * Caller-declared one-based frame index. Retrying the same accepted index
   * is idempotent; advancing it is the caller's acknowledgement boundary.
   */
  frameIndex: number;
  streamId: string;
}
