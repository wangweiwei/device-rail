/* eslint-disable */
/**
 * Generated from the checked-in DeviceRail JSON Schema. DO NOT EDIT.
 * Run `pnpm protocol:types:generate` from the repository root.
 */

export type Platform =
  | {
      kind: "web";
      [k: string]: unknown;
    }
  | {
      kind: "android";
      [k: string]: unknown;
    }
  | {
      kind: "ios";
      [k: string]: unknown;
    }
  | {
      kind: "harmonyOs";
      [k: string]: unknown;
    }
  | {
      kind: "macOs";
      [k: string]: unknown;
    }
  | {
      kind: "windows";
      [k: string]: unknown;
    }
  | {
      kind: "linux";
      [k: string]: unknown;
    }
  | {
      kind: "rdp";
      [k: string]: unknown;
    }
  | {
      kind: "mock";
      [k: string]: unknown;
    }
  | {
      kind: "other";
      value: string;
      [k: string]: unknown;
    };

export interface DeviceInfo {
  connected: boolean;
  id: string;
  name: string;
  osVersion?: string | null;
  platform: Platform;
  [k: string]: unknown;
}
