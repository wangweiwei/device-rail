/* eslint-disable */
/**
 * Generated from the checked-in DeviceRail JSON Schema. DO NOT EDIT.
 * Run `pnpm protocol:types:generate` from the repository root.
 */

export type RpcIdSchema = string | number;
export type JsonRpcVersion = "2.0";
export type MediaStreamEndMethodSchema = "media.stream.end";

export interface MediaStreamEndRequest {
  id: RpcIdSchema;
  jsonrpc: JsonRpcVersion;
  method: MediaStreamEndMethodSchema;
  params: MediaStreamEndParams;
}
/**
 * Parameters accepted by `media.stream.end`.
 */
export interface MediaStreamEndParams {
  streamId: string;
}
