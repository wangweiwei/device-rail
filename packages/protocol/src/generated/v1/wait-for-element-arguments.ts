/* eslint-disable */
/**
 * Generated from the checked-in DeviceRail JSON Schema. DO NOT EDIT.
 * Run `pnpm protocol:types:generate` from the repository root.
 */

export type UiContextKind = "native" | "web";

export interface WaitForElementArguments {
  condition?: "present" | "visible" | "enabled" | "absent";
  selector: ElementSelector;
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
