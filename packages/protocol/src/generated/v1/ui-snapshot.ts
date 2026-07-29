/* eslint-disable */
/**
 * Generated from the checked-in DeviceRail JSON Schema. DO NOT EDIT.
 * Run `pnpm protocol:types:generate` from the repository root.
 */

export type UiContextKind = "native" | "web";

/**
 * Canonical UI Tree. `nodes` is preorder and every parent must precede its
 * contiguous descendants. The serialized payload is bounded to 768 KiB.
 */
export interface UiSnapshot {
  context: UiContextRef;
  formatVersion: number;
  /**
   * @minItems 1
   * @maxItems 10000
   */
  nodes: [UiNode, ...UiNode[]];
  observationId: string;
  /**
   * @minItems 1
   * @maxItems 10000
   */
  rootStableNodeIds: [string, ...string[]];
}
/**
 * Full identity of one native accessibility or web-document context.
 * `documentEpoch` is required for both channels and changes after reconnect,
 * navigation, or any replacement that invalidates prior node references.
 */
export interface UiContextRef {
  contextId: string;
  contextKind: UiContextKind;
  documentEpoch: string;
}
/**
 * One node in the normalized preorder list. Unknown platform values remain
 * `null`; Drivers must not manufacture optimistic enabled/hittable states.
 */
export interface UiNode {
  bounds?: UiRect | null;
  enabled?: boolean | null;
  hittable?: boolean | null;
  identifier?: string | null;
  name?: string | null;
  parentStableNodeId?: string | null;
  role: string;
  stableNodeId: string;
  text?: string | null;
  value?: string | null;
}
export interface UiRect {
  height: number;
  width: number;
  x: number;
  y: number;
}
