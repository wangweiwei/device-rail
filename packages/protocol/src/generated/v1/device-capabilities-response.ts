/* eslint-disable */
/**
 * Generated from the checked-in DeviceRail JSON Schema. DO NOT EDIT.
 * Run `pnpm protocol:types:generate` from the repository root.
 */

export type DeviceCapabilitiesResponse = DeviceCapabilitiesSuccessSchema | SystemHelloFailureSchema;
export type RpcIdSchema = string | number;
export type JsonRpcVersion = "2.0";
export type ActionProtection = "standard" | "protected";
export type NullableRpcIdSchema = RpcIdSchema | null;

export interface DeviceCapabilitiesSuccessSchema {
  id: RpcIdSchema;
  jsonrpc: JsonRpcVersion;
  result: ActionDefinition[];
}
export interface ActionDefinition {
  description: string;
  inputSchema: unknown;
  name: string;
  protection?: ActionProtection;
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
