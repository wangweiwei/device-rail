/* eslint-disable */
/**
 * Generated from the checked-in DeviceRail JSON Schema. DO NOT EDIT.
 * Run `pnpm protocol:types:generate` from the repository root.
 */

export type SessionOutcome = "completed" | "failed" | "cancelled" | "shutdown";

/**
 * Optional parameters accepted by `session.end`.
 */
export interface SessionEndParams {
  outcome?: SessionOutcome | null;
  reason?: string | null;
}
