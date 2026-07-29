/* eslint-disable */
/**
 * Generated from the checked-in DeviceRail JSON Schema. DO NOT EDIT.
 * Run `pnpm protocol:types:generate` from the repository root.
 */

/**
 * A short-lived bearer URL. Debug intentionally never exposes its contents.
 */
export type EventStreamEndpoint = string;
/**
 * Identifies one daemon process lifetime. A cursor from another epoch must
 * never be accepted as a resumable position.
 */
export type EventStreamEpoch = string;

export interface EventsStreamOpenResult {
  endpoint: EventStreamEndpoint;
  expiresAtMs: number;
  streamEpoch: EventStreamEpoch;
}
