/* eslint-disable */
/**
 * Generated from the checked-in DeviceRail JSON Schema. DO NOT EDIT.
 * Run `pnpm protocol:types:generate` from the repository root.
 */

export type RpcIdSchema = string | number;
export type JsonRpcVersion = "2.0";
export type EventsClearMethodSchema = "events.clear";

export interface EventsClearRequest {
  id: RpcIdSchema;
  jsonrpc: JsonRpcVersion;
  method: EventsClearMethodSchema;
  params?: SessionTargetParams;
}
/**
 * Optional session selector accepted by `events.clear`.
 */
export interface SessionTargetParams {
  sessionId?: string | null;
}
