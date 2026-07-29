/* eslint-disable */
/**
 * Generated from the checked-in DeviceRail JSON Schema. DO NOT EDIT.
 * Run `pnpm protocol:types:generate` from the repository root.
 */

export type RpcIdSchema = string | number;
export type JsonRpcVersion = "2.0";
export type DeviceSelectMethodSchema = "device.select";

export interface DeviceSelectRequest {
  id: RpcIdSchema;
  jsonrpc: JsonRpcVersion;
  method: DeviceSelectMethodSchema;
  params: DeviceSelectParams;
}
/**
 * Parameters accepted by `device.select`.
 */
export interface DeviceSelectParams {
  deviceId: string;
}
