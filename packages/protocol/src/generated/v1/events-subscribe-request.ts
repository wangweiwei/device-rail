/* eslint-disable */
/**
 * Generated from the checked-in DeviceRail JSON Schema. DO NOT EDIT.
 * Run `pnpm protocol:types:generate` from the repository root.
 */

export type RpcIdSchema = string | number;
export type JsonRpcVersion = "2.0";
export type EventsSubscribeMethodSchema = "events.subscribe";
/**
 * A one-based sequence number within one session.
 *
 * The wire value is capped at JavaScript's maximum safe integer so generated
 * clients can sort and resume event streams without losing precision.
 */
export type EventSequence = number;
/**
 * Identifies one daemon process lifetime. A cursor from another epoch must
 * never be accepted as a resumable position.
 */
export type EventStreamEpoch = string;

export interface EventsSubscribeRequest {
  id: RpcIdSchema;
  jsonrpc: JsonRpcVersion;
  method: EventsSubscribeMethodSchema;
  params: EventsSubscribeParams;
}
export interface EventsSubscribeParams {
  afterCursor?: EventStreamCursor | null;
  sessionId: string;
}
/**
 * A Session-scoped, daemon-epoch-scoped application acknowledgement.
 */
export interface EventStreamCursor {
  sequence: EventSequence;
  sessionId: string;
  streamEpoch: EventStreamEpoch;
}
