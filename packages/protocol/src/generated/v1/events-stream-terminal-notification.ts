/* eslint-disable */
/**
 * Generated from the checked-in DeviceRail JSON Schema. DO NOT EDIT.
 * Run `pnpm protocol:types:generate` from the repository root.
 */

export type JsonRpcVersion = "2.0";
export type EventsStreamTerminalMethod = "events.stream.terminal";
/**
 * A one-based sequence number within one session.
 *
 * The wire value is capped at JavaScript's maximum safe integer so generated
 * clients can sort and resume event streams without losing precision.
 */
export type EventSequence = number;
/**
 * Identifies one daemon process lifetime. A cursor from another epoch must
 * never be accepted as a resumable position.
 */
export type EventStreamEpoch = string;
/**
 * The terminal reason is a closed tagged union so reason/error combinations
 * cannot become ambiguous on the wire.
 */
export type EventsStreamTermination =
  | {
      reason: "sessionEnded";
    }
  | {
      reason: "cancelled";
    }
  | {
      error: ErrorInfo;
      reason: "slowConsumer";
    }
  | {
      error: ErrorInfo;
      reason: "sessionDeleted";
    }
  | {
      error: ErrorInfo;
      reason: "serverShutdown";
    }
  | {
      error: ErrorInfo;
      reason: "sequenceGap";
    }
  | {
      error: ErrorInfo;
      reason: "eventTooLarge";
    }
  | {
      error: ErrorInfo;
      reason: "internalError";
    };

export interface EventsStreamTerminalNotification {
  jsonrpc: JsonRpcVersion;
  method: EventsStreamTerminalMethod;
  params: EventsStreamTerminalParams;
}
export interface EventsStreamTerminalParams {
  lastEmittedCursor?: EventStreamCursor | null;
  sessionId: string;
  subscriptionId: string;
  termination: EventsStreamTermination;
}
/**
 * A Session-scoped, daemon-epoch-scoped application acknowledgement.
 */
export interface EventStreamCursor {
  sequence: EventSequence;
  sessionId: string;
  streamEpoch: EventStreamEpoch;
}
export interface ErrorInfo {
  code: string;
  details?: unknown;
  message: string;
  retryable: boolean;
}
