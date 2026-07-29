/* eslint-disable */
/**
 * Generated from the checked-in DeviceRail JSON Schema. DO NOT EDIT.
 * Run `pnpm protocol:types:generate` from the repository root.
 */

export type RpcIdSchema = string | number;
export type RequestCancelStatus = "requested" | "alreadyRequested" | "notFound";

export interface RequestCancelResult {
  requestId: RpcIdSchema;
  status: RequestCancelStatus;
}
