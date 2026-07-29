/* eslint-disable */
/**
 * Generated from the checked-in DeviceRail JSON Schema. DO NOT EDIT.
 * Run `pnpm protocol:types:generate` from the repository root.
 */

/**
 * Parameters accepted by `media.stream.capture`.
 */
export interface MediaStreamCaptureParams {
  durationMs?: number | null;
  /**
   * Caller-declared one-based frame index. Retrying the same accepted index
   * is idempotent; advancing it is the caller's acknowledgement boundary.
   */
  frameIndex: number;
  streamId: string;
}
