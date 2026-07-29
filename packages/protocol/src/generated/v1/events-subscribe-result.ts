/* eslint-disable */
/**
 * Generated from the checked-in DeviceRail JSON Schema. DO NOT EDIT.
 * Run `pnpm protocol:types:generate` from the repository root.
 */

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
export type SessionState = "active" | "ended";

export interface EventsSubscribeResult {
  replayThrough: EventStreamCursor;
  sessionId: string;
  sessionState: SessionState;
  subscriptionId: string;
}
/**
 * A Session-scoped, daemon-epoch-scoped application acknowledgement.
 */
export interface EventStreamCursor {
  sequence: EventSequence;
  sessionId: string;
  streamEpoch: EventStreamEpoch;
}
