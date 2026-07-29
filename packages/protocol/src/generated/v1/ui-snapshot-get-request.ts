/* eslint-disable */
/**
 * Generated from the checked-in DeviceRail JSON Schema. DO NOT EDIT.
 * Run `pnpm protocol:types:generate` from the repository root.
 */

export type RpcIdSchema = string | number;
export type JsonRpcVersion = "2.0";
export type UiSnapshotGetMethodSchema = "ui.snapshot.get";

export interface UiSnapshotGetRequest {
  id: RpcIdSchema;
  jsonrpc: JsonRpcVersion;
  method: UiSnapshotGetMethodSchema;
  params: UiSnapshotGetParams;
}
/**
 * Parameters accepted by `ui.snapshot.get`.
 *
 * The active Session owns the Observation lookup; callers cannot name a
 * different Session or provide an arbitrary Evidence reference.
 */
export interface UiSnapshotGetParams {
  observationId: string;
}
