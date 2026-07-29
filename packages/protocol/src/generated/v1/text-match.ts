/* eslint-disable */
/**
 * Generated from the checked-in DeviceRail JSON Schema. DO NOT EDIT.
 * Run `pnpm protocol:types:generate` from the repository root.
 */

export interface TextMatch {
  caseSensitive?: boolean;
  mode?: "exact" | "contains";
  value: string;
}
