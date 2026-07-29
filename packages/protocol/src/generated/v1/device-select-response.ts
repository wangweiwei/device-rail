/* eslint-disable */
/**
 * Generated from the checked-in DeviceRail JSON Schema. DO NOT EDIT.
 * Run `pnpm protocol:types:generate` from the repository root.
 */

export type DeviceSelectResponse = DeviceSelectSuccessSchema | SystemHelloFailureSchema;
export type RpcIdSchema = string | number;
export type JsonRpcVersion = "2.0";
export type Platform =
  | {
      kind: "web";
      [k: string]: unknown;
    }
  | {
      kind: "android";
      [k: string]: unknown;
    }
  | {
      kind: "ios";
      [k: string]: unknown;
    }
  | {
      kind: "harmonyOs";
      [k: string]: unknown;
    }
  | {
      kind: "macOs";
      [k: string]: unknown;
    }
  | {
      kind: "windows";
      [k: string]: unknown;
    }
  | {
      kind: "linux";
      [k: string]: unknown;
    }
  | {
      kind: "rdp";
      [k: string]: unknown;
    }
  | {
      kind: "mock";
      [k: string]: unknown;
    }
  | {
      kind: "other";
      value: string;
      [k: string]: unknown;
    };
export type NullableRpcIdSchema = RpcIdSchema | null;

export interface DeviceSelectSuccessSchema {
  id: RpcIdSchema;
  jsonrpc: JsonRpcVersion;
  result: DeviceSelectResult;
}
/**
 * Result returned by `device.select`.
 */
export interface DeviceSelectResult {
  device: DeviceInfo;
}
export interface DeviceInfo {
  connected: boolean;
  id: string;
  name: string;
  osVersion?: string | null;
  platform: Platform;
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
