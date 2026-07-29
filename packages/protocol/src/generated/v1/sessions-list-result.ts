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
export type SessionState = "active" | "ended";
export type SessionsListResult = SessionInfo[];

export interface SessionInfo {
  endedAtMs?: number | null;
  eventCount: EventSequence;
  id: string;
  lastSequence: EventSequence;
  startedAtMs: number;
  state: SessionState;
}
