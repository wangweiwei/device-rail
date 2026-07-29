/* eslint-disable */
/**
 * Generated from the checked-in DeviceRail JSON Schema. DO NOT EDIT.
 * Run `pnpm protocol:types:generate` from the repository root.
 */

export type RpcIdSchema = string | number;
export type JsonRpcVersion = "2.0";
export type EventsStreamOpenMethodSchema = "events.stream.open";
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

export interface EventsStreamOpenRequest {
  id: RpcIdSchema;
  jsonrpc: JsonRpcVersion;
  method: EventsStreamOpenMethodSchema;
  params: EventsStreamOpenParams;
}
export interface EventsStreamOpenParams {
  originPolicy: EventStreamOriginPolicy;
  sessionId: string;
}
