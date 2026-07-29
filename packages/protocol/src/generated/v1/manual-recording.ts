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

/**
 * A bounded, Driver-neutral record of human-selected Actions.
 *
 * Protected Action arguments are represented only by an opaque host-owned
 * secret reference. The secret value is never part of this durable DTO.
 */
export interface ManualRecording {
  actionSpaceSha256: string;
  endedAtMs: number;
  formatVersion: number;
  recordingId: string;
  sourceDeviceId: string;
  startedAtMs: number;
  /**
   * @maxItems 10000
   */
  steps: ManualActionStep[];
}
export interface ManualActionStep {
  arguments: ManualActionArguments;
  callId: string;
  capturedAtMs: number;
  name: string;
  sequence: EventSequence;
}
