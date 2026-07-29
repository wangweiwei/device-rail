import type {
  ProtocolVersion,
  SessionInfo,
  TestEvent,
} from "@devicerail/protocol";

export const RECORDER_CHECKPOINT_FORMAT =
  "devicerail.execution-recorder-checkpoint" as const;
export const RECORDER_CHECKPOINT_VERSION = 1 as const;

export type RecorderPhase = "recording" | "sealed" | "completed";

/** Path-free receipt retained only after export and independent validation agree. */
export interface RecorderBundleReceipt {
  readonly sessionId: string;
  readonly eventCount: number;
  readonly assetCount: number;
  readonly assetBytes: number;
}

interface RecorderCheckpointBase {
  readonly format: typeof RECORDER_CHECKPOINT_FORMAT;
  readonly version: typeof RECORDER_CHECKPOINT_VERSION;
  readonly revision: number;
  readonly sessionId: string;
  readonly eventProtocolVersion: ProtocolVersion;
  readonly events: readonly TestEvent[];
}

export interface RecordingCheckpoint extends RecorderCheckpointBase {
  readonly phase: "recording";
  readonly session?: never;
  readonly bundle?: never;
}

export interface SealedCheckpoint extends RecorderCheckpointBase {
  readonly phase: "sealed";
  readonly session: SessionInfo;
  readonly bundle?: never;
}

export interface CompletedCheckpoint extends RecorderCheckpointBase {
  readonly phase: "completed";
  readonly session: SessionInfo;
  readonly bundle: RecorderBundleReceipt;
}

export type RecorderCheckpoint =
  | RecordingCheckpoint
  | SealedCheckpoint
  | CompletedCheckpoint;

export type EventAcceptance = "accepted" | "duplicate";

export interface EventBatchResult {
  readonly accepted: number;
  readonly duplicates: number;
  readonly lastSequence: number | null;
  readonly terminal: boolean;
}

export interface EventLogSnapshot {
  readonly sessionId: string;
  readonly events: readonly TestEvent[];
  readonly lastSequence: number | null;
  readonly terminal: boolean;
  readonly openActionCount: number;
}
