/* eslint-disable */
/**
 * Generated from the checked-in DeviceRail JSON Schema. DO NOT EDIT.
 * Run `pnpm protocol:types:generate` from the repository root.
 */

export type ElementTarget =
  | {
      kind: "selector";
      selector: ElementSelector;
    }
  | {
      kind: "node";
      node: UiNodeRef;
    };
export type UiContextKind = "native" | "web";

export interface SetElementValueArguments {
  target: ElementTarget;
  value: string;
}
/**
 * Cross-channel selector. Native contexts use accessibility fields; web
 * contexts may additionally use CSS. Context selection never carries a stale
 * document epoch; resolved node references always carry the full context.
 */
export interface ElementSelector {
  context?: UiContextSelector | null;
  css?: string | null;
  identifier?: string | null;
  name?: string | null;
  role?: string | null;
  text?: TextMatch | null;
  value?: string | null;
}
/**
 * Selects a current context without pretending to own its document epoch.
 */
export interface UiContextSelector {
  contextId?: string | null;
  contextKind: UiContextKind;
}
export interface TextMatch {
  caseSensitive?: boolean;
  mode?: "exact" | "contains";
  value: string;
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
