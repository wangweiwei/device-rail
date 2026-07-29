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
 * Optional parameters accepted by `session.export`.
 *
 * Omitting both cursor fields preserves the original complete-export
 * behavior. Supplying `limit` requests a bounded page after
 * `afterSequence`; `afterSequence` without `limit` is invalid at dispatch.
 */
export interface SessionExportParams {
  afterSequence?: EventSequence | null;
  limit?: number | null;
  sessionId?: string | null;
}
