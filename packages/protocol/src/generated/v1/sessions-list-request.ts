/* eslint-disable */
/**
 * Generated from the checked-in DeviceRail JSON Schema. DO NOT EDIT.
 * Run `pnpm protocol:types:generate` from the repository root.
 */

export type RpcIdSchema = string | number;
export type JsonRpcVersion = "2.0";
export type SessionsListMethodSchema = "sessions.list";
export type NoParamsSchema = EmptyParamsObjectSchema | [];

export interface SessionsListRequest {
  id: RpcIdSchema;
  jsonrpc: JsonRpcVersion;
  method: SessionsListMethodSchema;
  params?: NoParamsSchema;
}
export type EmptyParamsObjectSchema = Record<string, never>;
