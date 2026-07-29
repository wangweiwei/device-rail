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
 * Optional parameters accepted by `events.list`.
 */
export interface EventsListParams {
  afterSequence?: EventSequence | null;
  limit?: number | null;
  sessionId?: string | null;
}
