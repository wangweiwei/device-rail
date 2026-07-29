/* eslint-disable */
/**
 * Generated from the checked-in DeviceRail JSON Schema. DO NOT EDIT.
 * Run `pnpm protocol:types:generate` from the repository root.
 */

export type RpcIdSchema = string | number;
export type JsonRpcVersion = "2.0";
export type DeviceCapabilitiesMethodSchema = "device.capabilities";
export type NoParamsSchema = EmptyParamsObjectSchema | [];
/**
 * A positive timeout in milliseconds that is safe to represent as a JSON
 * number in every supported client language.
 */
export type RequestTimeoutMs = number;

export interface DeviceCapabilitiesRequest {
  id: RpcIdSchema;
  jsonrpc: JsonRpcVersion;
  method: DeviceCapabilitiesMethodSchema;
  params?: NoParamsSchema;
  timeoutMs?: RequestTimeoutMs;
}
export type EmptyParamsObjectSchema = Record<string, never>;
