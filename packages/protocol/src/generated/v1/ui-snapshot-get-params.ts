/* eslint-disable */
/**
 * Generated from the checked-in DeviceRail JSON Schema. DO NOT EDIT.
 * Run `pnpm protocol:types:generate` from the repository root.
 */

/**
 * Parameters accepted by `ui.snapshot.get`.
 *
 * The active Session owns the Observation lookup; callers cannot name a
 * different Session or provide an arbitrary Evidence reference.
 */
export interface UiSnapshotGetParams {
  observationId: string;
}
