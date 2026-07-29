export type RecorderErrorCode =
  | "action_call_reused"
  | "action_correlation_mismatch"
  | "action_in_flight"
  | "action_not_started"
  | "action_result_mismatch"
  | "bundle_cli_failed"
  | "bundle_summary_invalid"
  | "bundle_summary_mismatch"
  | "checkpoint_conflict"
  | "checkpoint_corrupt"
  | "checkpoint_durability_unknown"
  | "checkpoint_locked"
  | "duplicate_event_id"
  | "event_too_large"
  | "invalid_event"
  | "invalid_lifecycle"
  | "operation_cancelled"
  | "out_of_order"
  | "sequence_conflict"
  | "sequence_gap"
  | "session_export_mismatch"
  | "session_mismatch"
  | "session_not_ended"
  | "source_conflict"
  | "source_corrupt"
  | "source_durability_unknown"
  | "source_too_large"
  | "terminal_append"
  | "upstream_unavailable";

export interface RecorderErrorOptions extends ErrorOptions {
  readonly details?: Readonly<Record<string, unknown>>;
}

export class RecorderError extends Error {
  readonly code: RecorderErrorCode;
  readonly details: Readonly<Record<string, unknown>> | undefined;

  constructor(code: RecorderErrorCode, message: string, options: RecorderErrorOptions = {}) {
    super(message, options);
    this.name = "RecorderError";
    this.code = code;
    this.details = options.details;
  }
}
