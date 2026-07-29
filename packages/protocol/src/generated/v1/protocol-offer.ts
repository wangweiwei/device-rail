/* eslint-disable */
/**
 * Generated from the checked-in DeviceRail JSON Schema. DO NOT EDIT.
 * Run `pnpm protocol:types:generate` from the repository root.
 */

/**
 * An explicit set of supported ranges. Multiple entries avoid implying support
 * for major versions that a peer intentionally skipped.
 */
export interface ProtocolOffer {
  /**
   * @minItems 1
   */
  ranges: [ProtocolRange, ...ProtocolRange[]];
}
/**
 * A contiguous minor-version range within one protocol major version.
 */
export interface ProtocolRange {
  major: number;
  maxMinor: number;
  minMinor: number;
}
