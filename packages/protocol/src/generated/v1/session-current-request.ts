/* eslint-disable */
/**
 * Generated from the checked-in DeviceRail JSON Schema. DO NOT EDIT.
 * Run `pnpm protocol:types:generate` from the repository root.
 */

export type RpcIdSchema = string | number;
export type JsonRpcVersion = "2.0";
export type SessionCurrentMethodSchema = "session.current";
export type NoParamsSchema = EmptyParamsObjectSchema | [];

export interface SessionCurrentRequest {
  id: RpcIdSchema;
  jsonrpc: JsonRpcVersion;
  method: SessionCurrentMethodSchema;
  params?: NoParamsSchema;
}
export type EmptyParamsObjectSchema = Record<string, never>;
