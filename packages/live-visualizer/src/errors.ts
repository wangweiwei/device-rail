export type LiveTimelineErrorCode =
  | "event_conflict"
  | "invalid_confirmation"
  | "invalid_event"
  | "invalid_limits"
  | "invalid_page"
  | "pending_confirmation"
  | "sequence_gap"
  | "stale_prepared_event"
  | "timeline_closed"
  | "viewer_capacity_exceeded";

export class LiveTimelineError extends Error {
  readonly code: LiveTimelineErrorCode;
  readonly details: Readonly<Record<string, number | string>> | undefined;

  constructor(
    code: LiveTimelineErrorCode,
    message: string,
    options: { readonly details?: Readonly<Record<string, number | string>> } = {},
  ) {
    super(message);
    this.name = "LiveTimelineError";
    this.code = code;
    this.details = options.details ? Object.freeze({ ...options.details }) : undefined;
  }
}
