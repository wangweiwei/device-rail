/* eslint-disable */
/**
 * Generated from the checked-in DeviceRail JSON Schema. DO NOT EDIT.
 * Run `pnpm protocol:types:generate` from the repository root.
 */

export type VerdictStatus = "pass" | "fail" | "unknown";

/**
 * Parameters accepted by `verdict.record`.
 *
 * DeviceRail persists this caller-supplied Verdict; it does not infer or
 * upgrade the verdict status.
 */
export interface VerdictRecordParams {
  verdict: Verdict;
}
export interface Verdict {
  /**
   * @maxItems 64
   */
  evidence?: AssetRef[];
  status: VerdictStatus;
  summary: string;
}
export interface AssetRef {
  id: string;
  mediaType: string;
  sha256?: string | null;
  uri: string;
  [k: string]: unknown;
}
