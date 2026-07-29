/* eslint-disable */
/**
 * Generated from the checked-in DeviceRail JSON Schema. DO NOT EDIT.
 * Run `pnpm protocol:types:generate` from the repository root.
 */

/**
 * Browser Origin policy bound to a single-use stream capability.
 */
export type EventStreamOriginPolicy =
  | {
      kind: "absent";
    }
  | {
      kind: "exact";
      origin: string;
    };

export interface EventsStreamOpenParams {
  originPolicy: EventStreamOriginPolicy;
  sessionId: string;
}
