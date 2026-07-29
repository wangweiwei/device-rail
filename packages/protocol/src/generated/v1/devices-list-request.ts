/* eslint-disable */
/**
 * Generated from the checked-in DeviceRail JSON Schema. DO NOT EDIT.
 * Run `pnpm protocol:types:generate` from the repository root.
 */

export type RpcIdSchema = string | number;
export type JsonRpcVersion = "2.0";
export type DevicesListMethodSchema = "devices.list";
export type NoParamsSchema = EmptyParamsObjectSchema | [];

export interface DevicesListRequest {
  id: RpcIdSchema;
  jsonrpc: JsonRpcVersion;
  method: DevicesListMethodSchema;
  params?: NoParamsSchema;
}
export type EmptyParamsObjectSchema = Record<string, never>;
