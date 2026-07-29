/* eslint-disable */
/**
 * Generated from the checked-in DeviceRail JSON Schema. DO NOT EDIT.
 * Run `pnpm protocol:types:generate` from the repository root.
 */

/**
 * Identifies one daemon process lifetime. A cursor from another epoch must
 * never be accepted as a resumable position.
 */
export type EventStreamEpoch = string;
