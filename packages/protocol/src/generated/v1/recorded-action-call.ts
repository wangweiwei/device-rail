/* eslint-disable */
/**
 * Generated from the checked-in DeviceRail JSON Schema. DO NOT EDIT.
 * Run `pnpm protocol:types:generate` from the repository root.
 */

/**
 * Durable representation of an Action invocation.
 *
 * Standard calls preserve the historical wire shape. Protected and unknown
 * calls retain only correlation fields and serialize `arguments` as `null`
 * with an explicit `argumentsRedacted` marker.
 */
export interface RecordedActionCall {
  arguments?: unknown;
  argumentsRedacted?: boolean;
  id: string;
  name: string;
  [k: string]: unknown;
}
