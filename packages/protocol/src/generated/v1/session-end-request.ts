/* eslint-disable */
/**
 * Generated from the checked-in DeviceRail JSON Schema. DO NOT EDIT.
 * Run `pnpm protocol:types:generate` from the repository root.
 */

export type RpcIdSchema = string | number;
export type JsonRpcVersion = "2.0";
export type SessionEndMethodSchema = "session.end";
export type SessionOutcome = "completed" | "failed" | "cancelled" | "shutdown";

export interface SessionEndRequest {
  id: RpcIdSchema;
  jsonrpc: JsonRpcVersion;
  method: SessionEndMethodSchema;
  params?: SessionEndParams;
}
/**
 * Optional parameters accepted by `session.end`.
 */
export interface SessionEndParams {
  outcome?: SessionOutcome | null;
  reason?: string | null;
}
