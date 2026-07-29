/* eslint-disable */
/**
 * Generated from the checked-in DeviceRail JSON Schema. DO NOT EDIT.
 * Run `pnpm protocol:types:generate` from the repository root.
 */

/**
 * A contiguous minor-version range within one protocol major version.
 */
export interface ProtocolRange {
  major: number;
  maxMinor: number;
  minMinor: number;
}
