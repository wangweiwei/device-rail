/* eslint-disable */
/**
 * Generated from the checked-in DeviceRail JSON Schema. DO NOT EDIT.
 * Run `pnpm protocol:types:generate` from the repository root.
 */

export type WaitForElementCondition = "present" | "visible" | "enabled" | "absent";
export type UiContextKind = "native" | "web";

export interface WaitForElementResult {
  condition: WaitForElementCondition;
  element?: UiNodeRef | null;
  matched: boolean;
}
/**
 * Durable reference to one node in one observed UI Tree.
 */
export interface UiNodeRef {
  context: UiContextRef;
  observationId: string;
  stableNodeId: string;
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
