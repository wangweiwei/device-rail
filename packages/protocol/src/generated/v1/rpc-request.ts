/* eslint-disable */
/**
 * Generated from the checked-in DeviceRail JSON Schema. DO NOT EDIT.
 * Run `pnpm protocol:types:generate` from the repository root.
 */

export type RpcIdSchema = string | number;
export type JsonRpcVersion = "2.0";
export type RpcParams =
  | {
      [k: string]: unknown;
    }
  | unknown[];
/**
 * A positive timeout in milliseconds that is safe to represent as a JSON
 * number in every supported client language.
 */
export type RequestTimeoutMs = number;

export interface RpcRequest {
  id: RpcIdSchema;
  jsonrpc: JsonRpcVersion;
  method: string;
  params?: RpcParams;
  timeoutMs?: RequestTimeoutMs;
}
