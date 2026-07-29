/* eslint-disable */
/**
 * Generated from the checked-in DeviceRail JSON Schema. DO NOT EDIT.
 * Run `pnpm protocol:types:generate` from the repository root.
 */

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
