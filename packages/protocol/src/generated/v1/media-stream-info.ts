/* eslint-disable */
/**
 * Generated from the checked-in DeviceRail JSON Schema. DO NOT EDIT.
 * Run `pnpm protocol:types:generate` from the repository root.
 */

export type MediaStreamKind = "screenshot" | "video";

export interface MediaStreamInfo {
  id: string;
  kind: MediaStreamKind;
  mediaType: string;
  viewport?: Viewport | null;
}
export interface Viewport {
  height: number;
  scaleFactor: number;
  width: number;
  [k: string]: unknown;
}
