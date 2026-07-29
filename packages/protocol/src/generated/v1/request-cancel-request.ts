/* eslint-disable */
/**
 * Generated from the checked-in DeviceRail JSON Schema. DO NOT EDIT.
 * Run `pnpm protocol:types:generate` from the repository root.
 */

export type RpcIdSchema = string | number;
export type JsonRpcVersion = "2.0";
export type RequestCancelMethodSchema = "request.cancel";

export interface RequestCancelRequest {
  id: RpcIdSchema;
  jsonrpc: JsonRpcVersion;
  method: RequestCancelMethodSchema;
  params: RequestCancelParams;
}
export interface RequestCancelParams {
  requestId: RpcIdSchema;
}
