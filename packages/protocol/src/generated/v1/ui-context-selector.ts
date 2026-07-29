/* eslint-disable */
/**
 * Generated from the checked-in DeviceRail JSON Schema. DO NOT EDIT.
 * Run `pnpm protocol:types:generate` from the repository root.
 */

export type UiContextKind = "native" | "web";

/**
 * Selects a current context without pretending to own its document epoch.
 */
export interface UiContextSelector {
  contextId?: string | null;
  contextKind: UiContextKind;
}
