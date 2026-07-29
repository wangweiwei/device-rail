/* eslint-disable */
/**
 * Generated from the checked-in DeviceRail JSON Schema. DO NOT EDIT.
 * Run `pnpm protocol:types:generate` from the repository root.
 */

/**
 * A positive timeout in milliseconds that is safe to represent as a JSON
 * number in every supported client language.
 */
export type RequestTimeoutMs = number;

/**
 * Parameters for `device.execute`.
 *
 * The action fields intentionally remain flat on the wire. The optional
 * timeout controls only the Driver action, while the request envelope timeout
 * controls the request-scoped device-operation budget. Durable terminal event
 * finalization is shielded so cancellation cannot leave a half-open Action.
 */
export interface DeviceExecuteParams {
  actionTimeoutMs?: RequestTimeoutMs;
  arguments?: unknown;
  id: string;
  name: string;
}
