/* eslint-disable */
/**
 * Generated from the checked-in DeviceRail JSON Schema. DO NOT EDIT.
 * Run `pnpm protocol:types:generate` from the repository root.
 */

export type ManualActionArguments =
  | {
      kind: "captured";
      value: unknown;
    }
  | {
      kind: "protected";
      secretRef: string;
    };
/**
 * A one-based sequence number within one session.
 *
 * The wire value is capped at JavaScript's maximum safe integer so generated
 * clients can sort and resume event streams without losing precision.
 */
export type EventSequence = number;

export interface ManualActionStep {
  arguments: ManualActionArguments;
  callId: string;
  capturedAtMs: number;
  name: string;
  sequence: EventSequence;
}
