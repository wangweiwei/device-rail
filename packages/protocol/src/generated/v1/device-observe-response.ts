/* eslint-disable */
/**
 * Generated from the checked-in DeviceRail JSON Schema. DO NOT EDIT.
 * Run `pnpm protocol:types:generate` from the repository root.
 */

export type DeviceObserveResponse = DeviceObserveSuccessSchema | SystemHelloFailureSchema;
export type RpcIdSchema = string | number;
export type JsonRpcVersion = "2.0";
export type ScreenshotOmissionReason = "policy" | "protectedAction";
export type UiContextKind = "native" | "web";
export type UiSnapshotOmissionReason = "driverUnsupported" | "policy" | "protectedAction";
export type NullableRpcIdSchema = RpcIdSchema | null;

export interface DeviceObserveSuccessSchema {
  id: RpcIdSchema;
  jsonrpc: JsonRpcVersion;
  result: Observation;
}
export interface Observation {
  capturedAtMs: number;
  deviceId: string;
  id: string;
  metadata?: {
    [k: string]: unknown;
  };
  screenshot?: AssetRef | null;
  screenshotOmission?: ScreenshotOmissionReason | null;
  uiSnapshot?: UiSnapshotRef | null;
  uiSnapshotOmission?: UiSnapshotOmissionReason | null;
  viewport: Viewport;
  [k: string]: unknown;
}
export interface AssetRef {
  id: string;
  mediaType: string;
  sha256?: string | null;
  uri: string;
  [k: string]: unknown;
}
/**
 * Small Observation-side reference to a UI Tree Evidence object.
 */
export interface UiSnapshotRef {
  byteLength: number;
  context: UiContextRef;
  evidence: AssetRef;
  formatVersion: number;
  nodeCount: number;
}
/**
 * Full identity of one native accessibility or web-document context.
 * `documentEpoch` is required for both channels and changes after reconnect,
 * navigation, or any replacement that invalidates prior node references.
 */
export interface UiContextRef {
  contextId: string;
  contextKind: UiContextKind;
  documentEpoch: string;
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
